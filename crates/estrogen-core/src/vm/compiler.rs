//! OXC AST to stack bytecode, for the subset estrogen's VM can compile today.
//!
//! The compiler walks an OXC `Program` and emits the flat opcode array the
//! runtime runs. It does not reprint source: it produces a program object that
//! replaces the whole chunk (see [`super`]). A construct outside the subset
//! returns [`Unsupported`], and the caller falls back to the literal-pool and
//! name passes, the same way abulxt's shield degrades a chunk to `@no-vm`.
//!
//! The subset:
//!
//! - A module body of expression statements, `let`/`const` declarations with
//!   plain identifier bindings, and function declarations. Static imports and
//!   reexports use a native wrapper for ES module linking.
//! - Functions: declarations, anonymous function expressions, and arrow
//!   functions (expression or block body), with plain identifier parameters,
//!   `return`, recursion, nested functions, and lexical closure capture. `this`
//!   is the call-site receiver for a plain function and the defining `this` for
//!   an arrow.
//! - Control flow: `if`/`else`, `while`, `do`/`while`, C-style `for`, `switch`,
//!   and unlabeled `break`/`continue`.
//! - Expressions over literals and interpolated templates, identifiers, optional
//!   member access and calls, spread arguments and collections, `new`, unary
//!   and binary operators, conditional and logical operators, assignments, update
//!   to identifiers, array holes, object literals, and sequence expressions.
//! - Function-local `var` declarations, including block and loop hoisting.
//!   Module-root `var` is compiled only if module syntax proves ES module scope.
//!
//! Anything else (classes, locally declared exports, generators, `async`,
//! destructuring, default/rest parameters, named function expressions,
//! `arguments`, labeled break/continue, `try`/`catch`/`finally`, for-of/for-in,
//! top-level `var` without known module syntax, and update on a member) falls back.
//!
//! Locals are slot-based exactly as the runtime expects. A frame has one base
//! scope (its parameters and body-level lexicals); a lexical block pushes a
//! child scope (`ENTER_SCOPE`/`EXIT_SCOPE`). A name resolves to a packed
//! `(depth, index)` slot address, where `depth` counts scope hops up the runtime
//! chain, which spans the closure boundary because a closure's base scope points
//! at the defining frame's scope. A free name compiles to a `*_GLOBAL` op.

use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, AssignmentExpression, AssignmentOperator,
    AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty, BinaryOperator,
    BindingPattern, ChainElement, ChainExpression, Class, ClassElement, ComputedMemberExpression,
    Declaration, ExportDefaultDeclarationKind, Expression, ForStatement, ForStatementInit,
    ForStatementLeft, FormalParameters, Function, IfStatement, ImportOrExportKind,
    LabeledStatement, LogicalOperator, MemberExpression, MethodDefinitionKind, ModuleExportName,
    ObjectPropertyKind, Program, PropertyKey, PropertyKind, SimpleAssignmentTarget, Statement,
    StaticMemberExpression, SwitchStatement, TryStatement, UnaryOperator, UpdateOperator,
    VariableDeclarationKind,
};
use oxc_ast::AstKind;
use oxc_ast_visit::Visit;

use super::layout::Layout;
use super::program::{ConstPool, FunctionDef, Program as VmProgram};
use serde_json::json;

/// A construct estrogen's VM cannot compile yet. The caller treats it as a signal
/// to fall back, not as a build failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

type R<T> = Result<T, Unsupported>;

/// The body of a function being compiled: a statement list (declaration,
/// function expression, or block-body arrow) or a single expression (an arrow
/// with an expression body).
enum FnBody<'a> {
    Block(&'a [Statement<'a>]),
    Expr(&'a Expression<'a>),
}

/// Compile a parsed module into a VM program, or report the first construct
/// outside the subset. `seed` keys the opcode layout.
pub fn compile_program(program: &Program<'_>, seed: &str, known_module: bool) -> R<VmProgram> {
    let mut c = Compiler {
        layout: Layout::build(seed),
        consts: ConstPool::default(),
        functions: Vec::new(),
        scopes: Vec::new(),
        fns: Vec::new(),
        uses_import_meta: false,
        uses_dynamic_import: false,
        exports: Vec::new(),
        export_writes: Vec::new(),
        keep_names: names_observable(program),
    };
    let main_index = c.compile_main(program, known_module)?;
    Ok(VmProgram {
        layout: c.layout,
        consts: c.consts,
        functions: c.functions,
        main_index,
        uses_import_meta: c.uses_import_meta,
        uses_dynamic_import: c.uses_dynamic_import,
        exports: c.exports,
    })
}

/// A lexical scope: its slot names, indexed by slot number. One scope maps to one
/// runtime scope object.
#[derive(Default)]
struct Scope {
    names: Vec<String>,
}

/// Per-function emission state. A function has its own code array and its own
/// loop stack, so `break`/`continue` and jump targets never cross a function
/// boundary.
/// How a destructuring leaf stores its value: a lexical `let`/`const` binding
/// initializes its TDZ cell, a `var` binding stores into its hoisted slot.
#[derive(Clone, Copy)]
enum BindMode {
    Lexical,
    Var,
}

struct FnState {
    code: Vec<i64>,
    loops: Vec<LoopState>,
    is_module_main: bool,
    allow_module_var: bool,
    allow_root_class: bool,
    base_scope_index: usize,
    var_slots: Vec<(String, usize)>,
    /// The frame runs as an async function: the runtime wraps it in the async
    /// driver and returns a promise. Set from the source `async` keyword, and
    /// also raised by a top-level `await` in a module main so top-level await
    /// drives the module as a promise.
    is_async: bool,
    /// The frame is a generator body. `try`, `for`/`of`, and `for`/`in` decline
    /// inside it (they push handlers that complicate yield resume), and `yield`
    /// compiles only here.
    is_generator: bool,
    /// The live runtime handler stack (catch, finally, iterator), mirrored at
    /// compile time so an escaping `break`/`continue` knows what it crosses.
    handlers: Vec<HandlerKind>,
    /// The loop-stack length at the start of each finally body being compiled. A
    /// `break`/`continue` inside a finally that targets a loop below this escapes
    /// the finally and would abandon its pending completion record, which is not
    /// ported, so it declines. A `return` is fine: the frame leaves.
    finally_floors: Vec<usize>,
}

/// One open loop or `switch`. Collects the jump operand indices to patch once the
/// exit and continue targets are known, and records the scope depth each lands
/// at so a `break`/`continue` can unwind the block scopes it leaves.
struct LoopState {
    break_sites: Vec<usize>,
    continue_sites: Vec<usize>,
    break_depth: usize,
    continue_depth: usize,
    is_switch: bool,
    iterator: Option<IteratorLoop>,
    // The source label on the construct, if it was written `label: for (...)`.
    // `break label`/`continue label` resolve to the matching entry; `is_switch`
    // (true for a `switch` and for a labeled non-loop block) marks an entry that
    // `continue` cannot target.
    label: Option<String>,
    // The handler-stack depth when the loop body starts (a for-of sets it after
    // its own iterator handler). A jump out of the loop crosses the handlers
    // above this.
    handler_base: usize,
}

#[derive(Clone, Copy)]
struct IteratorLoop {
    slot: usize,
    head_scope_index: usize,
    handler_active: bool,
}

/// One live runtime handler, mirroring `frame.handlers` at compile time so a
/// `break`/`continue` that escapes a `try` knows what it crosses. A catch is
/// popped on the way out; a finally must run, so the jump routes through it;
/// a for-of iterator handler means the simple unwind does not apply.
#[derive(Clone, Copy)]
enum HandlerKind {
    Catch,
    /// The scope depth (`self.scopes.len()`) at the finally's `try`, so a jump
    /// that lands past the finally chain can exit the scopes left over.
    Finally {
        scope_depth: usize,
    },
    Iterator,
}

struct Compiler {
    layout: Layout,
    consts: ConstPool,
    /// Completed function definitions. `main_index` names the entry.
    functions: Vec<FunctionDef>,
    /// The scope chain, mirroring the runtime scope chain (across closures).
    scopes: Vec<Scope>,
    /// The function emission stack; the last entry is the one being compiled.
    fns: Vec<FnState>,
    /// Set when the module reads `import.meta` or uses dynamic `import()`, and
    /// the names the module exports from its own bindings (const/function/class),
    /// in declaration order. The wrapper uses these to pass the module context.
    uses_import_meta: bool,
    uses_dynamic_import: bool,
    /// The public names the module exports, in the order discovered. The wrapper
    /// re-exports each from the exports object the VM fills. A `default` entry is
    /// re-exported with `export default` instead of a const binding.
    exports: Vec<String>,
    /// Each deferred export writeback as `(local binding, public name)`. The main
    /// loads the local binding and stores it under the public name at the end of
    /// the run. An aliased `export { a as b }` loads `a` and exports `b`; a plain
    /// `export const a` or `export { a }` has the two the same. `export default`
    /// is written inline at its statement, so it is not in this list.
    export_writes: Vec<(String, String)>,
    /// Whether the chunk observes a callable's name. When false, every function
    /// and class name is dropped from the emitted program, so a decoded program
    /// carries no original identifier and `fn.name`/`constructor.name` read as
    /// the empty string, which nothing observes. When true, names are kept so
    /// reflection keeps working.
    keep_names: bool,
}

impl Compiler {
    // ---- function compilation ----

    fn compile_main(&mut self, program: &Program<'_>, known_module: bool) -> R<usize> {
        // Static import/reexport syntax proves a native ESM environment.
        // Root var bindings in that environment are module-local. With no
        // module syntax, a classic script may expose them on globalThis, so
        // retain the fallback until the caller supplies script kind.
        let allow_module_var = program.body.iter().any(|statement| {
            matches!(
                statement,
                Statement::ImportDeclaration(_)
                    | Statement::ExportAllDeclaration(_)
                    | Statement::ExportFromDeclaration(_)
                    | Statement::ExportNamedDeclaration(_)
                    | Statement::ExportDefaultDeclaration(_)
                    // A declaration-form export (`export const`, `export
                    // function`, `export class`) is this distinct variant, not
                    // ExportNamedDeclaration. It is module syntax too, so a chunk
                    // that carries only this form is still a module, where no
                    // top-level binding is a page global.
                    | Statement::ExportDeclaration(_)
            )
        });
        // A classic script's top-level declaration is a page global: a
        // `function`, `class`, or `var`/`let`/`const` bound at the program body
        // is visible to the other script tags on the page (one file defines
        // `Grid`, another reads it). The VM wrapper would scope that binding to
        // the wrapper, so the global vanishes and a multi-script app breaks. The
        // decline is enforced after the body compiles, once `import.meta` usage
        // is known (it proves module scope, where the binding is module-local).
        let has_top_level_declaration = program.body.iter().any(is_top_level_declaration);
        self.fns.push(FnState {
            code: Vec::new(),
            loops: Vec::new(),
            is_module_main: true,
            allow_module_var,
            allow_root_class: known_module || allow_module_var,
            base_scope_index: self.scopes.len(),
            var_slots: Vec::new(),
            is_async: false,
            is_generator: false,
            handlers: Vec::new(),
            finally_floors: Vec::new(),
        });
        self.scopes.push(Scope::default());
        if allow_module_var {
            self.predeclare_vars(&program.body)?;
        }
        self.compile_scope_body(&program.body, true)?;
        // Decline a classic script that binds a top-level global (see above). The
        // chunk is classic when it has neither module syntax, a caller-supplied
        // script kind, nor `import.meta` (whose presence proves module scope, so
        // the binding is module-local and safe to compile). `import.meta` usage
        // is known only now, after the body compiled.
        if has_top_level_declaration && !known_module && !allow_module_var && !self.uses_import_meta
        {
            return Err(Unsupported(
                "a classic script top-level declaration is a page global",
            ));
        }
        // After the body runs, write each exported binding's final value to the
        // module exports object the wrapper re-exports. A `const`/`let`/`function`
        // binding has settled by here; a `let` reassigned later is exported as its
        // final value, which the snapshot model approximates. `export default` is
        // not in this list; it was written inline at its own statement.
        for (local, public) in self.export_writes.clone() {
            self.emit_load_name(&local)?;
            let idx = self.consts.add_str(&public);
            self.emit1("EXPORT_BINDING", idx as i64);
        }
        self.emit0("HALT");
        self.scopes.pop();
        let st = self.fns.pop().expect("main frame");
        Ok(self.add_function(FunctionDef {
            code: st.code,
            name: None,
            is_async: st.is_async,
            is_arrow: false,
            is_generator: false,
            num_params: 0,
        }))
    }

    /// Compile a function body into a fresh `FunctionDef` and return its index.
    fn compile_function(
        &mut self,
        params: &FormalParameters<'_>,
        body: FnBody<'_>,
        name: Option<String>,
        is_arrow: bool,
        is_async: bool,
        is_generator: bool,
    ) -> R<usize> {
        self.fns.push(FnState {
            code: Vec::new(),
            loops: Vec::new(),
            is_module_main: false,
            allow_module_var: false,
            allow_root_class: false,
            base_scope_index: self.scopes.len(),
            var_slots: Vec::new(),
            is_async,
            is_generator,
            handlers: Vec::new(),
            finally_floors: Vec::new(),
        });
        self.scopes.push(Scope::default());
        let num_params = self.compile_params(params)?;
        // A non-arrow function that references `arguments` binds a local
        // arguments object from the call args. Arrows inherit it through the
        // scope chain, so they never bind their own.
        if !is_arrow {
            if let FnBody::Block(stmts) = &body {
                if references_arguments(stmts) && !params_bind_arguments(params) {
                    if body_binds_arguments(stmts) {
                        return Err(Unsupported("arguments shadowed by a local binding"));
                    }
                    let slot = self.add_local("arguments");
                    self.emit1("DECLARE_SLOT", (slot as i64) << 2);
                    self.emit0("LOAD_ARGUMENTS");
                    self.emit1("INIT_SLOT", pack_addr(0, slot));
                }
            }
        }
        if let FnBody::Block(stmts) = &body {
            self.predeclare_vars(stmts)?;
        }
        match body {
            FnBody::Block(stmts) => {
                self.compile_scope_body(stmts, false)?;
                self.emit0("RETURN_UNDEF");
            }
            FnBody::Expr(e) => {
                self.compile_expr(e)?;
                self.emit0("RETURN");
            }
        }
        self.scopes.pop();
        let st = self.fns.pop().expect("function frame");
        Ok(self.add_function(FunctionDef {
            code: st.code,
            name,
            is_async: st.is_async,
            is_arrow,
            is_generator: st.is_generator,
            num_params,
        }))
    }

    /// Bind the formal parameters into the fresh function scope and return the
    /// function's `length` (the count of leading plain parameters, stopping at
    /// the first default or rest, matching native `Function.prototype.length`).
    fn compile_params(&mut self, params: &FormalParameters<'_>) -> R<usize> {
        if params.items.len() > 0xff {
            return Err(Unsupported("too many parameters"));
        }
        let mut length = 0usize;
        let mut counting = true;
        for (i, item) in params.items.iter().enumerate() {
            match (&item.pattern, &item.initializer) {
                // Plain identifier, no default: bind the slot straight from the
                // argument. This counts toward Function.length.
                (BindingPattern::BindingIdentifier(id), None) => {
                    if counting {
                        length += 1;
                    }
                    let slot = self.add_local(id.name.as_str());
                    self.emit1("BIND_PARAM", ((slot as i64) << 8) | (i as i64));
                }
                // Identifier with a default: a default and everything after it
                // stops the length count.
                (BindingPattern::BindingIdentifier(id), Some(init)) => {
                    counting = false;
                    let slot = self.add_local(id.name.as_str());
                    self.emit1("DECLARE_SLOT", (slot as i64) << 2);
                    self.emit1("LOAD_ARG", i as i64);
                    self.apply_default(init)?;
                    self.emit1("INIT_SLOT", pack_addr(0, slot));
                }
                // Destructured parameter. A destructured param without a default
                // still counts toward Function.length; with one it stops the count.
                (pattern, initializer) => {
                    if initializer.is_some() {
                        counting = false;
                    } else if counting {
                        length += 1;
                    }
                    self.declare_pattern_slots(pattern, 0)?;
                    self.emit1("LOAD_ARG", i as i64);
                    if let Some(init) = initializer {
                        self.apply_default(init)?;
                    }
                    self.destructure_binding(pattern, BindMode::Lexical)?;
                }
            }
        }
        if let Some(rest) = &params.rest {
            // A rest parameter binds the arguments past the plain ones. The rest
            // target can itself be a pattern, so destructure the gathered array.
            let from = params.items.len() as i64;
            match &rest.rest.argument {
                BindingPattern::BindingIdentifier(id) => {
                    let slot = self.add_local(id.name.as_str());
                    self.emit1("DECLARE_SLOT", (slot as i64) << 2);
                    self.emit1("LOAD_ARGS_FROM", from);
                    self.emit1("INIT_SLOT", pack_addr(0, slot));
                }
                pattern => {
                    self.declare_pattern_slots(pattern, 0)?;
                    self.emit1("LOAD_ARGS_FROM", from);
                    self.destructure_binding(pattern, BindMode::Lexical)?;
                }
            }
        }
        Ok(length)
    }

    /// Declare a slot for every leaf of a binding pattern, so the following
    /// `destructure_binding` can initialize them. `kind` is the DECLARE_SLOT kind
    /// bit (0 for a mutable parameter slot, the lexical kind for a `const`/`let`
    /// head). Shared by destructured parameters and destructured `for`/`of` and
    /// `for`/`in` heads.
    fn declare_pattern_slots(&mut self, pattern: &BindingPattern<'_>, kind: i64) -> R<()> {
        let mut names = Vec::new();
        collect_pattern_names(pattern, &mut names)?;
        for name in &names {
            let slot = self.add_local(name);
            self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
        }
        Ok(())
    }

    fn add_function(&mut self, mut def: FunctionDef) -> usize {
        // When the chunk never observes a callable name, drop it: a decoded
        // program then carries no original identifier, and `fn.name` reads as the
        // empty string, which nothing in the chunk looks at.
        if !self.keep_names {
            def.name = None;
        }
        let idx = self.functions.len();
        self.functions.push(def);
        idx
    }

    /// A directly initialized binding or field gives an anonymous function,
    /// arrow, or class its own name. Other expressions retain their own names.
    fn compile_named_expr(&mut self, value: &Expression<'_>, name: &str) -> R<()> {
        match value {
            Expression::ClassExpression(class) if class.id.is_none() => {
                self.compile_class(class, Some(name), None)
            }
            Expression::FunctionExpression(func) if func.id.is_none() => {
                let body = func.body.as_ref().ok_or(Unsupported("function body"))?;
                let index = self.compile_function(
                    &func.params,
                    FnBody::Block(&body.statements),
                    Some(name.to_string()),
                    false,
                    func.r#async,
                    func.generator,
                )?;
                self.emit1("MAKE_FUNC", index as i64);
                Ok(())
            }
            Expression::ArrowFunctionExpression(arrow) => {
                let body = match &arrow.body {
                    ArrowFunctionBody::FunctionBody(body) => FnBody::Block(&body.statements),
                    _ => FnBody::Expr(arrow.get_expression().ok_or(Unsupported("arrow body"))?),
                };
                let index = self.compile_function(
                    &arrow.params,
                    body,
                    Some(name.to_string()),
                    true,
                    arrow.r#async,
                    false,
                )?;
                self.emit1("MAKE_FUNC", index as i64);
                Ok(())
            }
            _ => self.compile_expr(value),
        }
    }

    /// A field initializer is run with its receiver as `this` when the class is
    /// defined (static) or when an instance is allocated. Compiling it as its
    /// own function preserves closure capture, evaluation order and early TDZ.
    fn compile_field_initializer(&mut self, value: &Expression<'_>, name: &str) -> R<usize> {
        self.fns.push(FnState {
            code: Vec::new(),
            loops: Vec::new(),
            is_module_main: false,
            allow_module_var: false,
            allow_root_class: false,
            base_scope_index: self.scopes.len(),
            var_slots: Vec::new(),
            is_async: false,
            is_generator: false,
            handlers: Vec::new(),
            finally_floors: Vec::new(),
        });
        self.scopes.push(Scope::default());
        self.compile_named_expr(value, name)?;
        self.emit0("RETURN");
        self.scopes.pop();
        let state = self.fns.pop().expect("field frame");
        Ok(self.add_function(FunctionDef {
            code: state.code,
            name: None,
            is_async: false,
            is_arrow: false,
            is_generator: false,
            num_params: 0,
        }))
    }

    /// A `static { ... }` block runs once at class definition time with the
    /// class as `this`, in source order with the static fields. Compile it as a
    /// nullary function whose body is the block; `BUILD_CLASS` runs it and
    /// discards the result.
    fn compile_static_block(&mut self, stmts: &[Statement<'_>]) -> R<usize> {
        self.fns.push(FnState {
            code: Vec::new(),
            loops: Vec::new(),
            handlers: Vec::new(),
            finally_floors: Vec::new(),
            is_module_main: false,
            allow_module_var: false,
            allow_root_class: false,
            base_scope_index: self.scopes.len(),
            var_slots: Vec::new(),
            is_async: false,
            is_generator: false,
        });
        self.scopes.push(Scope::default());
        self.predeclare_vars(stmts)?;
        self.compile_scope_body(stmts, false)?;
        self.emit0("RETURN_UNDEF");
        self.scopes.pop();
        let state = self.fns.pop().expect("static block frame");
        Ok(self.add_function(FunctionDef {
            code: state.code,
            name: None,
            is_async: false,
            is_arrow: false,
            is_generator: false,
            num_params: 0,
        }))
    }

    /// Only public, non-derived classes enter the VM. The class descriptor
    /// holds function indices rather than values; BUILD_CLASS creates all
    /// methods and initializes the class binding before evaluating static fields.
    fn compile_class(
        &mut self,
        class: &Class<'_>,
        inferred_name: Option<&str>,
        bind: Option<i64>,
    ) -> R<()> {
        if !class.decorators.is_empty() {
            return Err(Unsupported("decorated class"));
        }
        let has_super = class.heritage.is_some();
        let name = class
            .id
            .as_ref()
            .map_or(inferred_name.unwrap_or(""), |id| id.name.as_str());
        let mut constructor = None;
        let mut methods = Vec::new();
        let mut fields = Vec::new();
        let mut priv_names: Vec<String> = Vec::new();
        // Computed member keys, in source order. Each computed method or field
        // records an index into this list; the key expressions are evaluated at
        // class-definition time into one array that BUILD_CLASS_DYN reads.
        let mut computed_keys: Vec<&Expression<'_>> = Vec::new();
        for element in &class.body.body {
            match element {
                ClassElement::MethodDefinition(method) => {
                    if !method.decorators.is_empty() {
                        return Err(Unsupported("decorated class method"));
                    }
                    let body = method
                        .value
                        .body
                        .as_ref()
                        .ok_or(Unsupported("class method body"))?;
                    if let MethodDefinitionKind::Constructor = method.kind {
                        if constructor.is_some() || method.r#static {
                            return Err(Unsupported("class constructor kind"));
                        }
                        constructor = Some(self.compile_function(
                            &method.value.params,
                            FnBody::Block(&body.statements),
                            None,
                            false,
                            false,
                            false,
                        )?);
                    } else {
                        // A plain method, a getter, or a setter. All compile to a
                        // function; `kind` tells BUILD_CLASS whether to install a
                        // value or an accessor on the prototype or the class.
                        let kind = match method.kind {
                            MethodDefinitionKind::Method => "method",
                            MethodDefinitionKind::Get => "get",
                            MethodDefinitionKind::Set => "set",
                            MethodDefinitionKind::Constructor => unreachable!(),
                        };
                        // A computed key is evaluated at class definition and read
                        // from the keys array by index. A computed getter or setter
                        // key works the same way; the runtime installs the accessor
                        // under the evaluated key, including a symbol key.
                        let (key, ckey) = if method.computed {
                            let ke = method
                                .key
                                .as_expression()
                                .ok_or(Unsupported("class method key expression"))?;
                            let idx = computed_keys.len();
                            computed_keys.push(ke);
                            (serde_json::Value::Null, Some(idx))
                        } else {
                            let k = static_key(&method.key)
                                .ok_or(Unsupported("non-static class method key"))?;
                            (serde_json::Value::String(k), None)
                        };
                        // A getter or setter is never async or generator (that is
                        // a syntax error), so only a plain method carries either
                        // flag; the body compiles through the same async and
                        // generator machinery a standalone function uses.
                        let index = self.compile_function(
                            &method.value.params,
                            FnBody::Block(&body.statements),
                            None,
                            false,
                            method.value.r#async,
                            method.value.generator,
                        )?;
                        let mut entry = json!({
                            "key": key,
                            "static": method.r#static,
                            "fn": index,
                            "kind": kind,
                        });
                        if let Some(idx) = ckey {
                            entry["ckey"] = json!(idx);
                        }
                        methods.push(entry);
                    }
                }
                ClassElement::PropertyDefinition(field) => {
                    if !field.decorators.is_empty() {
                        return Err(Unsupported("decorated class field"));
                    }
                    // A private instance field lives in the class's per-field
                    // WeakMap, not as a string property. Private static fields are
                    // not ported.
                    if let PropertyKey::PrivateIdentifier(id) = &field.key {
                        if field.r#static {
                            return Err(Unsupported("private static field"));
                        }
                        let pn = format!("#{}", id.name);
                        let initializer = match &field.value {
                            Some(value) => Some(self.compile_field_initializer(value, &pn)?),
                            None => None,
                        };
                        priv_names.push(pn.clone());
                        fields.push(
                            json!({"key": null, "static": false, "priv": pn, "fn": initializer}),
                        );
                        continue;
                    }
                    let (key, ckey, init_name) = if field.computed {
                        let ke = field
                            .key
                            .as_expression()
                            .ok_or(Unsupported("class field key expression"))?;
                        let idx = computed_keys.len();
                        computed_keys.push(ke);
                        // The initializer's inferred name would be the computed
                        // key, not known statically. An anonymous function value
                        // then names as the empty string; the chunk drops names
                        // anyway unless it observes one.
                        (serde_json::Value::Null, Some(idx), String::new())
                    } else {
                        let k = static_key(&field.key)
                            .ok_or(Unsupported("non-static class field key"))?;
                        (serde_json::Value::String(k.clone()), None, k)
                    };
                    let initializer = match &field.value {
                        Some(value) => Some(self.compile_field_initializer(value, &init_name)?),
                        None => None,
                    };
                    let mut entry =
                        json!({"key": key, "static": field.r#static, "fn": initializer});
                    if let Some(idx) = ckey {
                        entry["ckey"] = json!(idx);
                    }
                    fields.push(entry);
                }
                ClassElement::StaticBlock(block) => {
                    let index = self.compile_static_block(&block.body)?;
                    fields.push(json!({"key": null, "static": true, "fn": index, "block": true}));
                }
                _ => return Err(Unsupported("class accessor or private member")),
            }
        }
        // Drop the class name too when nothing in the chunk observes a callable
        // name, so the decoded descriptor carries no original identifier and
        // `constructor.name` reads as the empty string.
        let descriptor = json!({
            "name": if self.keep_names { name } else { "" },
            "bind": bind,
            "ctor": constructor,
            "methods": methods,
            "fields": fields,
            "hasSuper": has_super,
            "privNames": priv_names,
        });
        let index = self.consts.add_class(descriptor);
        // Push the superclass value (null for a non-derived class) first, since a
        // class heritage evaluates before the body's computed keys, for
        // BUILD_CLASS to pop.
        match class.heritage.as_ref() {
            Some(heritage) => self.compile_expr(&heritage.expression)?,
            None => self.emit0("PUSH_NULL"),
        }
        if computed_keys.is_empty() {
            self.emit1("BUILD_CLASS", index as i64);
        } else {
            // Evaluate each computed key in source order into one array on top of
            // the superclass; BUILD_CLASS_DYN pops the array, then the superclass.
            self.emit0("ARR_EMPTY");
            for key_expr in &computed_keys {
                self.compile_expr(key_expr)?;
                self.emit0("ARR_APPEND");
            }
            self.emit1("BUILD_CLASS_DYN", index as i64);
        }
        Ok(())
    }

    /// Compile the value of an `export default` declaration onto the stack. A
    /// function or class declares no module binding here (its name is only
    /// self-referential), so it compiles as a value; every other form is an
    /// expression.
    fn compile_export_default(&mut self, decl: &ExportDefaultDeclarationKind<'_>) -> R<()> {
        match decl {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                let fb = func
                    .body
                    .as_ref()
                    .ok_or(Unsupported("function without a body"))?;
                let name = func.id.as_ref().map(|id| id.name.as_str().to_string());
                let idx = self.compile_function(
                    &func.params,
                    FnBody::Block(&fb.statements),
                    name,
                    false,
                    func.r#async,
                    func.generator,
                )?;
                self.emit1("MAKE_FUNC", idx as i64);
                Ok(())
            }
            ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                // A named default class binds its own name in a wrapper scope for
                // self-reference, the same as a named class expression.
                if let Some(id) = &class.id {
                    self.open_scope();
                    let slot = self.add_local(id.name.as_str());
                    self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | 2);
                    self.compile_class(class, None, Some(pack_addr(0, slot)))?;
                    self.close_scope();
                } else {
                    self.compile_class(class, None, None)?;
                }
                Ok(())
            }
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {
                Err(Unsupported("export default interface"))
            }
            _ => {
                let expr = decl
                    .as_expression()
                    .ok_or(Unsupported("export default declaration"))?;
                self.compile_expr(expr)
            }
        }
    }

    fn predeclare_vars(&mut self, stmts: &[Statement<'_>]) -> R<()> {
        let mut var_names = Vec::new();
        collect_function_vars(stmts, &mut var_names)?;
        for name in var_names {
            let existing = self
                .scopes
                .last()
                .expect("base scope")
                .names
                .iter()
                .position(|n| n == &name);
            let slot = match existing {
                Some(slot) => slot,
                None => {
                    let slot = self.add_local(&name);
                    self.emit1("DECLARE_SLOT", (slot as i64) << 2);
                    slot
                }
            };
            self.fns
                .last_mut()
                .expect("frame")
                .var_slots
                .push((name, slot));
        }
        Ok(())
    }

    /// Compile a statement list in the current scope: declare its lexical slots
    /// (TDZ cells) and hoist its function declarations, then compile the
    /// statements in order. `top_level` is true only for the module body, where
    /// the last expression statement sets the module completion value.
    fn compile_scope_body(&mut self, stmts: &[Statement<'_>], top_level: bool) -> R<()> {
        // Declare lexical slots and collect function declarations to hoist.
        let mut hoisted: Vec<(usize, &Function<'_>)> = Vec::new();
        for stmt in stmts {
            match stmt {
                Statement::VariableDeclaration(decl) => {
                    if decl.kind == VariableDeclarationKind::Var {
                        // Var bindings live in the function base scope and
                        // were instantiated before the body is evaluated.
                        continue;
                    }
                    let kind = lexical_kind(decl.kind)?;
                    for d in &decl.declarations {
                        let mut names = Vec::new();
                        collect_pattern_names(&d.id, &mut names)?;
                        for name in names {
                            let slot = self.add_local(&name);
                            self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                        }
                    }
                }
                Statement::FunctionDeclaration(func) => {
                    let name = func
                        .id
                        .as_ref()
                        .ok_or(Unsupported("function declaration without a name"))?
                        .name
                        .as_str()
                        .to_string();
                    let slot = self
                        .scopes
                        .last()
                        .expect("scope")
                        .names
                        .iter()
                        .position(|existing| existing == &name)
                        .unwrap_or_else(|| self.add_local(&name));
                    hoisted.push((slot, func));
                }
                Statement::ClassDeclaration(class) => {
                    if self.root_class_needs_module_mode() {
                        return Err(Unsupported("top-level class needs module kind"));
                    }
                    let name = class
                        .id
                        .as_ref()
                        .ok_or(Unsupported("anonymous class declaration"))?;
                    let slot = self.add_local(name.name.as_str());
                    self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | 1);
                }
                Statement::ExportDeclaration(ex) if top_level => match &ex.declaration {
                    // `export function h() {}` hoists like a plain function
                    // declaration and records `h` as an export the wrapper writes
                    // back after the body runs.
                    Declaration::FunctionDeclaration(func) => {
                        let name = func
                            .id
                            .as_ref()
                            .ok_or(Unsupported("export function without a name"))?
                            .name
                            .as_str()
                            .to_string();
                        let slot = self
                            .scopes
                            .last()
                            .expect("scope")
                            .names
                            .iter()
                            .position(|existing| existing == &name)
                            .unwrap_or_else(|| self.add_local(&name));
                        hoisted.push((slot, func));
                        self.exports.push(name.clone());
                        self.export_writes.push((name.clone(), name));
                    }
                    // `export class C {}` declares its slot here and compiles into
                    // it in the statement pass, like a plain class declaration.
                    Declaration::ClassDeclaration(class) => {
                        if self.root_class_needs_module_mode() {
                            return Err(Unsupported("top-level class needs module kind"));
                        }
                        let name = class
                            .id
                            .as_ref()
                            .ok_or(Unsupported("anonymous class declaration"))?
                            .name
                            .as_str()
                            .to_string();
                        let slot = self.add_local(&name);
                        self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | 1);
                        self.exports.push(name.clone());
                        self.export_writes.push((name.clone(), name));
                    }
                    _ => {
                        let decl = export_const_declaration(ex)?;
                        let kind = lexical_kind(decl.kind)?;
                        for d in &decl.declarations {
                            let id = binding_identifier(&d.id)?;
                            let slot = self.add_local(id.name.as_str());
                            self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                            self.exports.push(id.name.as_str().to_string());
                            self.export_writes
                                .push((id.name.as_str().to_string(), id.name.as_str().to_string()));
                        }
                    }
                },
                _ => {}
            }
        }
        // Define the hoisted functions into their slots (they are callable before
        // their textual position, and a function can reference itself).
        for (slot, func) in hoisted {
            let name = func.id.as_ref().map(|id| id.name.as_str().to_string());
            let fb = func
                .body
                .as_ref()
                .ok_or(Unsupported("function without a body"))?;
            let idx = self.compile_function(
                &func.params,
                FnBody::Block(&fb.statements),
                name,
                false,
                func.r#async,
                func.generator,
            )?;
            self.emit1("MAKE_FUNC", idx as i64);
            self.emit1("DEF_FUNC_SLOT", slot as i64);
        }
        // Statements in order. Function declarations are already defined, and so
        // is an exported function declaration, which was hoisted above.
        for stmt in stmts {
            if matches!(stmt, Statement::FunctionDeclaration(_)) {
                continue;
            }
            if let Statement::ExportDeclaration(ex) = stmt {
                if matches!(&ex.declaration, Declaration::FunctionDeclaration(_)) {
                    continue;
                }
            }
            self.compile_statement(stmt, top_level)?;
        }
        Ok(())
    }

    // ---- statements ----

    fn compile_statement(&mut self, stmt: &Statement<'_>, top_level: bool) -> R<()> {
        match stmt {
            // The native ESM wrapper keeps static imports and supplies their
            // live bindings to this VM frame as read-only globals.
            Statement::ImportDeclaration(_) if top_level => Ok(()),
            // Reexports have no local bindings or executable body. Native ESM
            // linking remains responsible for their live export bindings.
            Statement::ExportAllDeclaration(_) if top_level => Ok(()),
            Statement::ExportFromDeclaration(_) if top_level => Ok(()),
            // A local `export const <bindings>` compiles its declaration here;
            // the slot was declared in the hoist pass and the name recorded for
            // the wrapper. The main emits EXPORT_BINDING for each at the end.
            Statement::ExportDeclaration(ex) if top_level => match &ex.declaration {
                // The exported function was already defined in the hoist pass.
                Declaration::FunctionDeclaration(_) => Ok(()),
                // `export class C {}`: compile into the slot declared in the hoist
                // pass, like a plain class declaration.
                Declaration::ClassDeclaration(class) => {
                    let name = class
                        .id
                        .as_ref()
                        .ok_or(Unsupported("anonymous class declaration"))?;
                    let (depth, slot) = self
                        .resolve(name.name.as_str())
                        .ok_or(Unsupported("class binding"))?;
                    self.compile_class(class, None, Some(pack_addr(depth, slot)))?;
                    self.emit0("POP");
                    Ok(())
                }
                _ => {
                    let decl = export_const_declaration(ex)?;
                    for d in &decl.declarations {
                        let id = binding_identifier(&d.id)?;
                        let (_, slot) = self
                            .resolve(id.name.as_str())
                            .ok_or(Unsupported("export binding not declared"))?;
                        let init = d
                            .init
                            .as_ref()
                            .ok_or(Unsupported("export const without an initializer"))?;
                        self.compile_named_expr(init, id.name.as_str())?;
                        self.emit1("INIT_SLOT", pack_addr(0, slot));
                    }
                    Ok(())
                }
            },
            Statement::EmptyStatement(_) => Ok(()),
            Statement::ExpressionStatement(s) => {
                self.compile_expr(&s.expression)?;
                if top_level {
                    // The last top-level expression statement sets the module
                    // completion value, which `run()` returns.
                    self.emit0("SET_COMPLETION");
                } else {
                    self.emit0("POP");
                }
                Ok(())
            }
            Statement::VariableDeclaration(decl) => {
                if decl.kind == VariableDeclarationKind::Var {
                    let frame = self.fns.last().expect("frame");
                    if frame.is_module_main && !frame.allow_module_var {
                        return Err(Unsupported("top-level var needs a script kind"));
                    }
                    for d in &decl.declarations {
                        match &d.id {
                            BindingPattern::BindingIdentifier(id) => {
                                if let Some(init) = &d.init {
                                    self.compile_var_init(id.name.as_str(), init)?;
                                }
                            }
                            _ => {
                                let init = d.init.as_ref().ok_or(Unsupported(
                                    "destructuring var without an initializer",
                                ))?;
                                self.compile_expr(init)?;
                                self.destructure_binding(&d.id, BindMode::Var)?;
                            }
                        }
                    }
                    return Ok(());
                }
                lexical_kind(decl.kind)?;
                for d in &decl.declarations {
                    let id = match &d.id {
                        BindingPattern::BindingIdentifier(id) => id,
                        _ => {
                            let init = d.init.as_ref().ok_or(Unsupported(
                                "destructuring binding without an initializer",
                            ))?;
                            self.compile_expr(init)?;
                            self.destructure_binding(&d.id, BindMode::Lexical)?;
                            continue;
                        }
                    };
                    if d.init
                        .as_ref()
                        .is_some_and(|expr| matches!(expr, Expression::ClassExpression(_)))
                        && self.root_class_needs_module_mode()
                    {
                        return Err(Unsupported("top-level class initializer needs module kind"));
                    }
                    let (depth, slot) = self
                        .resolve(id.name.as_str())
                        .ok_or(Unsupported("binding not declared in its scope"))?;
                    debug_assert_eq!(depth, 0, "a declaration initializes a current-scope slot");
                    match &d.init {
                        Some(init) => self.compile_named_expr(init, id.name.as_str())?,
                        None => self.emit0("PUSH_UNDEF"),
                    }
                    self.emit1("INIT_SLOT", pack_addr(0, slot));
                }
                Ok(())
            }
            Statement::BlockStatement(b) => self.compile_block(&b.body),
            Statement::ClassDeclaration(class) => {
                let name = class
                    .id
                    .as_ref()
                    .ok_or(Unsupported("anonymous class declaration"))?;
                let (depth, slot) = self
                    .resolve(name.name.as_str())
                    .ok_or(Unsupported("class binding"))?;
                self.compile_class(class, None, Some(pack_addr(depth, slot)))?;
                self.emit0("POP");
                Ok(())
            }
            Statement::IfStatement(s) => self.compile_if(s),
            Statement::WhileStatement(s) => self.compile_while(&s.test, &s.body, None),
            Statement::DoWhileStatement(s) => self.compile_do_while(&s.body, &s.test, None),
            Statement::ForStatement(s) => self.compile_for(s, None),
            Statement::ForOfStatement(s) if !s.r#await => {
                self.compile_iterated(&s.left, &s.right, &s.body, false, None)
            }
            Statement::ForInStatement(s) => {
                self.compile_iterated(&s.left, &s.right, &s.body, true, None)
            }
            Statement::SwitchStatement(s) => self.compile_switch(s, None),
            Statement::LabeledStatement(s) => self.compile_labeled(s),
            Statement::TryStatement(s) => self.compile_try(s),
            Statement::ThrowStatement(s) => {
                self.compile_expr(&s.argument)?;
                self.emit0("THROW");
                Ok(())
            }
            Statement::ReturnStatement(s) => {
                if self.fns.last().expect("frame").is_module_main {
                    return Err(Unsupported("return at the top level"));
                }
                match &s.argument {
                    Some(arg) => self.compile_expr(arg)?,
                    None => self.emit0("PUSH_UNDEF"),
                }
                self.close_iterators_for_return()?;
                self.emit0("RETURN");
                Ok(())
            }
            Statement::BreakStatement(s) => match &s.label {
                Some(l) => self.emit_break_label(l.name.as_str()),
                None => self.emit_break(),
            },
            Statement::ContinueStatement(s) => match &s.label {
                Some(l) => self.emit_continue_label(l.name.as_str()),
                None => self.emit_continue(),
            },
            Statement::FunctionDeclaration(_) => Err(Unsupported(
                "function declaration outside a statement-list top",
            )),
            // A local `export { a, b as c }` list. The specifiers name bindings
            // already declared in this module; record each as a deferred export
            // writeback that the main emits at the end, mapping the local binding
            // to its public name. A re-export (`export { x } from 'm'`) is a
            // separate statement the native wrapper keeps; this arm is only the
            // local list, which carries no `source`.
            Statement::ExportNamedDeclaration(decl) if top_level => {
                if decl.export_kind != ImportOrExportKind::Value {
                    // `export type { T }` emits nothing at runtime.
                    return Ok(());
                }
                for spec in &decl.specifiers {
                    if spec.export_kind != ImportOrExportKind::Value {
                        continue;
                    }
                    let local = module_export_name(&spec.local)?;
                    let public = module_export_name(&spec.exported)?;
                    self.exports.push(public.to_string());
                    self.export_writes
                        .push((local.to_string(), public.to_string()));
                }
                Ok(())
            }
            // `export default <expr|function|class>`. Evaluate the value here, in
            // module order, and write it to the exports object under `default`.
            Statement::ExportDefaultDeclaration(ex) if top_level => {
                self.compile_export_default(&ex.declaration)?;
                let idx = self.consts.add_str("default");
                self.emit1("EXPORT_BINDING", idx as i64);
                self.exports.push("default".to_string());
                Ok(())
            }
            Statement::ForOfStatement(_) => {
                Err(Unsupported("for-await-of loop outside the subset"))
            }
            Statement::WithStatement(_) => Err(Unsupported("with statement outside the subset")),
            Statement::DebuggerStatement(_) => {
                Err(Unsupported("debugger statement outside the subset"))
            }
            _ => Err(Unsupported("statement outside the subset")),
        }
    }

    fn compile_block(&mut self, stmts: &[Statement<'_>]) -> R<()> {
        let needs_scope = block_needs_scope(stmts);
        if needs_scope {
            self.open_scope();
        }
        self.compile_scope_body(stmts, false)?;
        if needs_scope {
            self.close_scope();
        }
        Ok(())
    }

    fn compile_try(&mut self, stmt: &TryStatement<'_>) -> R<()> {
        // A try inside a generator leaves a handler on the frame across a yield,
        // and a consumer's return/throw at that yield would need to run the
        // finally and iterator cleanup. That is not ported. A try with no yield
        // or await inside it never suspends while its handler is live, so it
        // pushes and pops within one resume like an ordinary function and
        // compiles; one that can suspend inside declines.
        if self.fns.last().expect("frame").is_generator && try_statement_suspends(stmt) {
            return Err(Unsupported("try in a generator"));
        }
        // A return that leaves a try while a for-of iterator is still open needs
        // multi-iterator unwinding that is not ported; decline for both the
        // catch and finally forms.
        if contains_return_stmt(&stmt.block.body)
            && self
                .fns
                .last()
                .expect("frame")
                .loops
                .iter()
                .any(|loop_state| {
                    loop_state
                        .iterator
                        .is_some_and(|iterator| iterator.handler_active)
                })
        {
            return Err(Unsupported("return inside try in iterated loop"));
        }
        let catch = stmt.handler.as_ref();
        let finalizer = stmt.finalizer.as_ref();
        if catch.is_none() && finalizer.is_none() {
            return Err(Unsupported("try without catch or finally"));
        }
        // A break/continue/return that escapes the try is routed by emit_break,
        // emit_continue, and doReturn: a catch it crosses is popped, a finally it
        // crosses runs first (through ABRUPT_JUMP). The compile-time handler
        // stack below mirrors the runtime handlers so those jumps know what they
        // cross.

        // `finally` pushes a handler that stays live through the body and the
        // catch, so an abrupt completion routes its completion record through
        // the finally block before leaving.
        let try_scope_depth = self.scopes.len();
        let finally_handler = finalizer.map(|_| self.emit_jump("PUSH_FINALLY"));
        if finalizer.is_some() {
            self.push_handler(HandlerKind::Finally {
                scope_depth: try_scope_depth,
            });
        }

        if let Some(catch) = catch {
            let catch_target = self.emit_jump("PUSH_HANDLER");
            self.push_handler(HandlerKind::Catch);
            self.compile_block(&stmt.block.body)?;
            self.emit0("POP_HANDLER");
            self.pop_handler();
            let after_catch = self.emit_jump("JMP");
            self.patch_here(catch_target);

            // The handler enters after unwinding the try block. An exception is
            // left on the stack, ready to initialize the optional catch binding.
            self.open_scope();
            if let Some(parameter) = &catch.param {
                let name = binding_identifier(&parameter.pattern)?;
                let slot = self.add_local(name.name.as_str());
                self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | 1);
                self.emit1("INIT_SLOT", pack_addr(0, slot));
            } else {
                self.emit0("POP");
            }
            self.compile_scope_body(&catch.body.body, false)?;
            self.close_scope();
            self.patch_here(after_catch);
        } else {
            self.compile_block(&stmt.block.body)?;
        }

        if let (Some(finally_handler), Some(finalizer)) = (finally_handler, finalizer) {
            // Normal completion: drop the finally handler, record a normal
            // completion, and fall into the finally body. An abrupt completion
            // jumped here with its own record already pushed.
            self.emit0("POP_HANDLER");
            self.pop_handler();
            self.emit0("PUSH_COMPLETION_NORMAL");
            self.patch_here(finally_handler);
            let floor = self.fns.last().expect("frame").loops.len();
            self.fns
                .last_mut()
                .expect("frame")
                .finally_floors
                .push(floor);
            self.compile_block(&finalizer.body)?;
            self.fns.last_mut().expect("frame").finally_floors.pop();
            let end = self.emit_jump("END_FINALLY");
            self.patch_here(end);
        }
        Ok(())
    }

    fn compile_if(&mut self, s: &IfStatement<'_>) -> R<()> {
        self.compile_expr(&s.test)?;
        let to_else = self.emit_jump("JMP_IF_FALSE");
        self.compile_statement(&s.consequent, false)?;
        if let Some(alt) = &s.alternate {
            let to_end = self.emit_jump("JMP");
            self.patch_here(to_else);
            self.compile_statement(alt, false)?;
            self.patch_here(to_end);
        } else {
            self.patch_here(to_else);
        }
        Ok(())
    }

    fn compile_while(
        &mut self,
        test: &Expression<'_>,
        body: &Statement<'_>,
        label: Option<&str>,
    ) -> R<()> {
        let l_top = self.code_len();
        let depth = self.scopes.len();
        self.push_loop(depth, depth, false, label);
        self.compile_expr(test)?;
        let exit = self.emit_jump("JMP_IF_FALSE");
        self.compile_statement(body, false)?;
        self.emit1("JMP", l_top as i64);
        let l_end = self.code_len();
        self.patch_to(exit, l_end);
        let lp = self.pop_loop();
        self.patch_all(&lp.break_sites, l_end);
        self.patch_all(&lp.continue_sites, l_top);
        Ok(())
    }

    fn compile_do_while(
        &mut self,
        body: &Statement<'_>,
        test: &Expression<'_>,
        label: Option<&str>,
    ) -> R<()> {
        let l_top = self.code_len();
        let depth = self.scopes.len();
        self.push_loop(depth, depth, false, label);
        self.compile_statement(body, false)?;
        let l_cont = self.code_len();
        self.compile_expr(test)?;
        self.emit1("JMP_IF_TRUE", l_top as i64);
        let l_end = self.code_len();
        let lp = self.pop_loop();
        self.patch_all(&lp.break_sites, l_end);
        self.patch_all(&lp.continue_sites, l_cont);
        Ok(())
    }

    fn compile_for(&mut self, s: &ForStatement<'_>, label: Option<&str>) -> R<()> {
        let lexical_head = s
            .init
            .as_ref()
            .is_some_and(ForStatementInit::is_lexical_declaration);
        if lexical_head {
            // A `for (let ...)` binding is fresh per iteration, so a closure made
            // in the body or the test captures that iteration's value. When one
            // can, lower to per-iteration environments (`compile_for_captured`)
            // instead of the single-binding fast path below.
            let test_captures = s.test.as_ref().is_some_and(|e| has_closure_expr(e));
            let update_captures = s.update.as_ref().is_some_and(|e| has_closure_expr(e));
            let body_captures = has_closure_stmt(&s.body);
            if test_captures || update_captures || body_captures {
                // The update runs in the canonical scope, not the per-iteration
                // one the spec evaluates it in, so a closure in the update would
                // capture a different environment. That case is rare; decline it
                // rather than miscompile.
                if update_captures {
                    return Err(Unsupported(
                        "for-loop update closure captures the loop binding",
                    ));
                }
                return self.compile_for_captured(s, label);
            }
            self.open_scope();
        }
        let break_depth = self.scopes.len() - usize::from(lexical_head);

        match &s.init {
            None => {}
            Some(ForStatementInit::VariableDeclaration(decl)) => {
                if decl.kind == VariableDeclarationKind::Var {
                    let frame = self.fns.last().expect("frame");
                    if frame.is_module_main && !frame.allow_module_var {
                        return Err(Unsupported("top-level var needs a script kind"));
                    }
                    for d in &decl.declarations {
                        let id = binding_identifier(&d.id)?;
                        if let Some(init) = &d.init {
                            self.compile_var_init(id.name.as_str(), init)?;
                        }
                    }
                } else {
                    let kind = lexical_kind(decl.kind)?;
                    let mut slots = Vec::with_capacity(decl.declarations.len());
                    for d in &decl.declarations {
                        let id = binding_identifier(&d.id)?;
                        let slot = self.add_local(id.name.as_str());
                        self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                        slots.push((slot, d.init.as_ref()));
                    }
                    for (slot, init) in slots {
                        match init {
                            Some(e) => self.compile_expr(e)?,
                            None => self.emit0("PUSH_UNDEF"),
                        }
                        self.emit1("INIT_SLOT", pack_addr(0, slot));
                    }
                }
            }
            Some(other) => {
                let e = other
                    .as_expression()
                    .ok_or(Unsupported("for-init expression"))?;
                self.compile_expr(e)?;
                self.emit0("POP");
            }
        }

        let l_test = self.code_len();
        let continue_depth = self.scopes.len();
        self.push_loop(break_depth, continue_depth, false, label);
        let exit = match &s.test {
            Some(t) => {
                self.compile_expr(t)?;
                Some(self.emit_jump("JMP_IF_FALSE"))
            }
            None => None,
        };
        self.compile_statement(&s.body, false)?;
        let l_cont = self.code_len();
        if let Some(u) = &s.update {
            self.compile_expr(u)?;
            self.emit0("POP");
        }
        self.emit1("JMP", l_test as i64);
        if let Some(exit) = exit {
            self.patch_here(exit);
        }
        if lexical_head {
            self.close_scope();
        }
        let l_end = self.code_len();
        let lp = self.pop_loop();
        self.patch_all(&lp.break_sites, l_end);
        self.patch_all(&lp.continue_sites, l_cont);
        Ok(())
    }

    /// Lower a `for (let ...; test; update)` whose body or test makes a closure
    /// that captures the loop binding. A canonical scope holds the running
    /// values; each iteration opens a fresh child scope, copies the bindings in,
    /// runs the test and body there (so a closure captures that iteration's
    /// copy), copies any mutation back, exits the child, then runs the update in
    /// the canonical scope. This gives each iteration its own binding, matching
    /// `CreatePerIterationEnvironment`, while the C-style update still carries the
    /// value forward. Only identifier bindings are supported, like the fast path.
    fn compile_for_captured(&mut self, s: &ForStatement<'_>, label: Option<&str>) -> R<()> {
        let Some(ForStatementInit::VariableDeclaration(decl)) = &s.init else {
            unreachable!("compile_for_captured requires a lexical for-init")
        };
        let kind = lexical_kind(decl.kind)?;
        let mut names = Vec::with_capacity(decl.declarations.len());
        for d in &decl.declarations {
            let id = binding_identifier(&d.id)?;
            names.push((id.name.as_str(), d.init.as_ref()));
        }

        let outer_base = self.scopes.len();
        // Canonical scope: holds the running value of each binding between
        // iterations, so the update can carry it forward.
        self.open_scope();
        let mut canon_slots = Vec::with_capacity(names.len());
        for (name, _) in &names {
            let slot = self.add_local(name);
            self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
            canon_slots.push(slot);
        }
        for (i, (_, init)) in names.iter().enumerate() {
            match init {
                Some(e) => self.compile_expr(e)?,
                None => self.emit0("PUSH_UNDEF"),
            }
            self.emit1("INIT_SLOT", pack_addr(0, canon_slots[i]));
        }

        let break_depth = outer_base;
        let l_test = self.code_len();
        // Per-iteration scope: a fresh binding a body closure captures.
        self.open_scope();
        let mut iter_slots = Vec::with_capacity(names.len());
        for (name, _) in &names {
            let slot = self.add_local(name);
            self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
            iter_slots.push(slot);
        }
        // Copy each binding in from the canonical scope (one level out).
        for i in 0..names.len() {
            self.emit1("LOAD_SLOT", pack_addr(1, canon_slots[i]));
            self.emit1("INIT_SLOT", pack_addr(0, iter_slots[i]));
        }
        let continue_depth = self.scopes.len();
        self.push_loop(break_depth, continue_depth, false, label);
        let exit = match &s.test {
            Some(t) => {
                self.compile_expr(t)?;
                Some(self.emit_jump("JMP_IF_FALSE"))
            }
            None => None,
        };
        self.compile_statement(&s.body, false)?;
        let l_cont = self.code_len();
        // Carry any body mutation back to the canonical scope. A `const` binding
        // cannot be reassigned, so its copy never diverges and needs no write.
        if kind == 1 {
            for i in 0..names.len() {
                self.emit1("LOAD_SLOT", pack_addr(0, iter_slots[i]));
                self.emit1("STORE_SLOT", pack_addr(1, canon_slots[i]));
                self.emit0("POP");
            }
        }
        self.close_scope();
        if let Some(u) = &s.update {
            self.compile_expr(u)?;
            self.emit0("POP");
        }
        self.emit1("JMP", l_test as i64);
        // Test-false path: the per-iteration scope is still open at runtime (the
        // compiler already accounts it closed), so exit it before the canonical.
        if let Some(exit) = exit {
            self.patch_here(exit);
            self.emit0("EXIT_SCOPE");
        }
        let lp = self.pop_loop();
        self.patch_all(&lp.continue_sites, l_cont);
        self.close_scope();
        let l_end = self.code_len();
        self.patch_all(&lp.break_sites, l_end);
        Ok(())
    }

    fn compile_iterated(
        &mut self,
        left: &ForStatementLeft<'_>,
        right: &Expression<'_>,
        body: &Statement<'_>,
        for_in: bool,
        label: Option<&str>,
    ) -> R<()> {
        // A for-of/in inside a generator keeps an iterator handler on the frame
        // across a yield; a consumer return/throw there would need to close it.
        // Not ported. The handler is live only while the body runs, so a body
        // with no yield or await inside never suspends under it and compiles like
        // an ordinary function's loop; a body that can suspend declines.
        if self.fns.last().expect("frame").is_generator && statement_suspends(body) {
            return Err(Unsupported("for-of or for-in in a generator"));
        }
        // The loop head is either a fresh binding declaration or an existing
        // assignment target. `for (let x of it)` declares a per-iteration
        // binding; `for (obj.k of it)`, `for (x of it)` with an outer `x`, and a
        // destructuring head like `for ([a, b] of pairs)` store the yielded value
        // into a place that already exists, so no fresh binding is made.
        let (decl, target) = match left {
            ForStatementLeft::VariableDeclaration(decl) => (Some(decl), None),
            _ => (
                None,
                Some(
                    left.as_assignment_target()
                        .ok_or(Unsupported("iterated head kind"))?,
                ),
            ),
        };
        let (head, head_ident, lexical) = if let Some(decl) = decl {
            if decl.declarations.len() != 1 || decl.declarations[0].init.is_some() {
                return Err(Unsupported("iterated binding declaration"));
            }
            let head = &decl.declarations[0].id;
            let head_ident = match head {
                BindingPattern::BindingIdentifier(id) => Some(id.name.as_str()),
                _ => None,
            };
            let lexical = match decl.kind {
                VariableDeclarationKind::Var => false,
                VariableDeclarationKind::Let | VariableDeclarationKind::Const => true,
                _ => return Err(Unsupported("iterated declaration kind")),
            };
            (Some(head), head_ident, lexical)
        } else {
            (None, None, false)
        };
        // A top-level `var` head in a chunk of unknown script kind would expose a
        // page global. An assignment target writes a place that already exists,
        // so it is exempt from that rule.
        if decl.is_some() && !lexical {
            let frame = self.fns.last().expect("frame");
            if frame.is_module_main && !frame.allow_module_var {
                return Err(Unsupported("top-level var needs script kind"));
            }
        }
        self.open_scope();
        let head_scope_index = self.scopes.len() - 1;
        if lexical {
            // The RHS resolves its head binding to this uninitialized slot.
            // Every subsequent iteration gets a separate initialized scope, so
            // these outer slots are shadowed and never read.
            let head = head.expect("lexical head");
            match head_ident {
                Some(name) => {
                    let slot = self.add_local(name);
                    let kind = lexical_kind(decl.expect("lexical decl").kind)?;
                    self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                }
                None => self.declare_pattern_slots(head, 0)?,
            }
        }
        let iterator = self.add_local("\0iterator");
        self.emit1("DECLARE_SLOT", (iterator as i64) << 2);
        self.compile_expr(right)?;
        if for_in {
            self.emit0("FOR_IN_KEYS");
        }
        self.emit0("GET_ITERATOR");
        self.emit1("INIT_SLOT", pack_addr(0, iterator));

        let top = self.code_len();
        self.emit1("LOAD_SLOT", pack_addr(0, iterator));
        let normal_end = self.emit_jump("FOR_ITER");
        let depth = self.scopes.len();
        self.push_loop(depth - 1, depth, false, label);
        self.fns
            .last_mut()
            .expect("frame")
            .loops
            .last_mut()
            .expect("loop")
            .iterator = Some(IteratorLoop {
            slot: iterator,
            head_scope_index,
            handler_active: false,
        });
        if lexical {
            self.open_scope();
            let head = head.expect("lexical head");
            let kind = lexical_kind(decl.expect("lexical decl").kind)?;
            match head_ident {
                Some(name) => {
                    let slot = self.add_local(name);
                    self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                    self.emit1("INIT_SLOT", pack_addr(0, slot));
                }
                None => {
                    self.declare_pattern_slots(head, kind)?;
                    self.destructure_binding(head, BindMode::Lexical)?;
                }
            }
        } else if let Some(head) = head {
            // A `var` declaration head stores the yielded value by binding.
            match head_ident {
                Some(name) => {
                    self.emit_store_name(name);
                    self.emit0("POP");
                }
                None => self.destructure_binding(head, BindMode::Var)?,
            }
        } else {
            // An assignment target head stores the yielded value into the place
            // that already exists (a member, an outer binding, or a destructuring
            // pattern over existing targets).
            self.assign_leaf(target.expect("assignment target"))?;
        }

        let caught = self.emit_jump("PUSH_HANDLER");
        self.push_handler(HandlerKind::Iterator);
        // The loop body runs above its own iterator handler, so a jump out of
        // the body does not count that handler among the ones it crosses.
        let body_handler_base = self.fns.last().expect("frame").handlers.len();
        {
            let frame = self.fns.last_mut().expect("frame");
            let loop_state = frame.loops.last_mut().expect("loop");
            loop_state.handler_base = body_handler_base;
            loop_state
                .iterator
                .as_mut()
                .expect("iterator")
                .handler_active = true;
        }
        self.compile_statement(body, false)?;
        self.fns
            .last_mut()
            .expect("frame")
            .loops
            .last_mut()
            .expect("loop")
            .iterator
            .as_mut()
            .expect("iterator")
            .handler_active = false;
        self.emit0("POP_HANDLER");
        self.pop_handler();
        let after_catch = self.emit_jump("JMP");
        self.patch_here(caught);
        let error_slot = self.add_local("\0iteration-error");
        self.emit1("DECLARE_SLOT", (error_slot as i64) << 2);
        self.emit1("INIT_SLOT", pack_addr(0, error_slot));
        let iter_depth = self.scopes.len() - depth;
        self.emit1("LOAD_SLOT", pack_addr(iter_depth, iterator));
        self.emit0("DUP");
        let close = self.consts.add_str("close");
        self.emit1("GET_PROP", close as i64);
        self.emit1("LOAD_SLOT", pack_addr(0, error_slot));
        self.emit0("PUSH_TRUE");
        self.emit1("CALL_METHOD", 2);
        self.emit0("POP");
        self.emit1("LOAD_SLOT", pack_addr(0, error_slot));
        self.emit0("THROW");
        self.patch_here(after_catch);
        if lexical {
            self.close_scope();
        }
        self.emit1("JMP", top as i64);
        let normal_exit = self.code_len();
        self.patch_to(normal_end, normal_exit);
        self.close_scope();
        let break_exit = self.code_len();
        let loop_state = self.pop_loop();
        self.patch_all(&loop_state.break_sites, break_exit);
        self.patch_all(&loop_state.continue_sites, top);
        Ok(())
    }

    fn compile_switch(&mut self, s: &SwitchStatement<'_>, label: Option<&str>) -> R<()> {
        // The discriminant lives in a temp slot, not on the operand stack, so a
        // `break`/`continue` out of the switch never leaves a stray value.
        self.open_scope();
        let temp = self.add_local("*switch*");
        self.emit1("DECLARE_SLOT", (temp as i64) << 2);
        self.compile_expr(&s.discriminant)?;
        self.emit1("INIT_SLOT", pack_addr(0, temp));

        // The whole case block is one lexical scope the cases share. Declare its
        // `let`/`const`/`class` bindings up front as TDZ cells (after the
        // discriminant, so a discriminant that shadows one still reads the outer
        // binding), so a declaration in any case resolves to a slot and a case
        // that reaches it initializes it. A `var` is hoisted at the function
        // level already. A function declaration in a case has Annex B hoisting
        // the VM does not model, so decline it.
        for case in &s.cases {
            for stmt in &case.consequent {
                match stmt {
                    Statement::VariableDeclaration(decl)
                        if decl.kind != VariableDeclarationKind::Var =>
                    {
                        let kind = lexical_kind(decl.kind)?;
                        for d in &decl.declarations {
                            let mut names = Vec::new();
                            collect_pattern_names(&d.id, &mut names)?;
                            for name in names {
                                let slot = self.add_local(&name);
                                self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                            }
                        }
                    }
                    Statement::ClassDeclaration(class) => {
                        if self.root_class_needs_module_mode() {
                            return Err(Unsupported("top-level class needs module kind"));
                        }
                        let name = class
                            .id
                            .as_ref()
                            .ok_or(Unsupported("anonymous class declaration"))?;
                        let slot = self.add_local(name.name.as_str());
                        self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | 1);
                    }
                    Statement::FunctionDeclaration(_) => {
                        return Err(Unsupported("function declaration in a switch case"));
                    }
                    _ => {}
                }
            }
        }

        let mut body_sites: Vec<(usize, usize)> = Vec::new();
        let mut default_case: Option<usize> = None;
        for (ci, case) in s.cases.iter().enumerate() {
            match &case.test {
                Some(t) => {
                    self.emit1("LOAD_SLOT", pack_addr(0, temp));
                    self.compile_expr(t)?;
                    self.emit0("SEQ");
                    let site = self.emit_jump("JMP_IF_TRUE");
                    body_sites.push((ci, site));
                }
                None => default_case = Some(ci),
            }
        }
        let to_default = self.emit_jump("JMP");

        let depth = self.scopes.len();
        self.push_loop(depth, depth, true, label);
        let mut body_pos = vec![0usize; s.cases.len()];
        for (ci, case) in s.cases.iter().enumerate() {
            body_pos[ci] = self.code_len();
            for stmt in &case.consequent {
                self.compile_statement(stmt, false)?;
            }
        }
        let l_end = self.code_len();
        for (ci, site) in body_sites {
            self.patch_to(site, body_pos[ci]);
        }
        match default_case {
            Some(di) => self.patch_to(to_default, body_pos[di]),
            None => self.patch_to(to_default, l_end),
        }
        let lp = self.pop_loop();
        self.patch_all(&lp.break_sites, l_end);
        self.close_scope();
        Ok(())
    }

    /// Compile `label: <statement>`. When the body is a loop or `switch`, the
    /// label rides on that construct's loop entry, so `break label` and (for a
    /// loop) `continue label` resolve to it. When the body is anything else, the
    /// label names a breakable block: `break label` jumps past the statement, and
    /// `continue label` is a syntax error the parser already rejects.
    fn compile_labeled(&mut self, s: &LabeledStatement<'_>) -> R<()> {
        let label = s.label.name.as_str();
        match &s.body {
            Statement::WhileStatement(w) => self.compile_while(&w.test, &w.body, Some(label)),
            Statement::DoWhileStatement(w) => self.compile_do_while(&w.body, &w.test, Some(label)),
            Statement::ForStatement(f) => self.compile_for(f, Some(label)),
            Statement::ForOfStatement(f) if !f.r#await => {
                self.compile_iterated(&f.left, &f.right, &f.body, false, Some(label))
            }
            Statement::ForInStatement(f) => {
                self.compile_iterated(&f.left, &f.right, &f.body, true, Some(label))
            }
            Statement::SwitchStatement(sw) => self.compile_switch(sw, Some(label)),
            other => {
                let depth = self.scopes.len();
                self.push_loop(depth, depth, true, Some(label));
                self.compile_statement(other, false)?;
                let lp = self.pop_loop();
                let l_end = self.code_len();
                self.patch_all(&lp.break_sites, l_end);
                debug_assert!(
                    lp.continue_sites.is_empty(),
                    "continue cannot target a non-loop label",
                );
                Ok(())
            }
        }
    }

    // ---- expressions ----

    fn compile_expr(&mut self, expr: &Expression<'_>) -> R<()> {
        match expr {
            Expression::NumericLiteral(lit) => {
                let i = self.consts.add_num(lit.value);
                self.emit1("PUSH_CONST", i as i64);
            }
            Expression::StringLiteral(lit) => {
                let v = lit
                    .value
                    .as_str()
                    .ok_or(Unsupported("string literal with a lone surrogate"))?;
                let i = self.consts.add_str(v);
                self.emit1("PUSH_CONST", i as i64);
            }
            Expression::BooleanLiteral(lit) => {
                self.emit0(if lit.value { "PUSH_TRUE" } else { "PUSH_FALSE" });
            }
            Expression::NullLiteral(_) => self.emit0("PUSH_NULL"),
            Expression::RegExpLiteral(lit) => {
                let index = self.consts.add_regexp(
                    lit.regex.pattern.text.as_str(),
                    &lit.regex.flags.to_string(),
                );
                self.emit1("PUSH_CONST", index as i64);
            }
            Expression::BigIntLiteral(lit) => {
                let raw = lit.raw.as_ref().map_or("", |a| a.as_str());
                let digits = raw.trim_end_matches('n').replace('_', "");
                if digits.is_empty() {
                    return Err(Unsupported("bigint literal with no raw text"));
                }
                let i = self.consts.add_bigint(&digits);
                self.emit1("PUSH_CONST", i as i64);
            }
            Expression::Identifier(id) => self.emit_load_name(id.name.as_str())?,
            Expression::ThisExpression(_) => self.emit0("PUSH_THIS"),
            Expression::ParenthesizedExpression(p) => self.compile_expr(&p.expression)?,
            Expression::ChainExpression(chain) => self.compile_chain(chain)?,
            Expression::SequenceExpression(seq) => {
                let n = seq.expressions.len();
                for (i, e) in seq.expressions.iter().enumerate() {
                    self.compile_expr(e)?;
                    if i + 1 < n {
                        self.emit0("POP");
                    }
                }
            }
            Expression::TemplateLiteral(tpl) => {
                if tpl.quasis.len() != tpl.expressions.len() + 1 {
                    return Err(Unsupported("template quasi count"));
                }
                for (index, quasi) in tpl.quasis.iter().enumerate() {
                    if index != 0 {
                        // The internal helper converts with string hint and
                        // rejects Symbol values, as template substitution
                        // does. JS addition alone uses the default hint.
                        let helper = self.consts.add_str("\0t");
                        self.emit1("LOAD_GLOBAL", helper as i64);
                        self.compile_expr(&tpl.expressions[index - 1])?;
                        self.emit1("CALL", 1);
                        self.emit0("ADD");
                    }
                    let cooked = quasi
                        .value
                        .cooked
                        .and_then(|c| c.as_str())
                        .ok_or(Unsupported("template with no cooked value"))?;
                    let i = self.consts.add_str(cooked);
                    self.emit1("PUSH_CONST", i as i64);
                    if index != 0 {
                        self.emit0("ADD");
                    }
                }
            }
            Expression::UnaryExpression(u) => self.compile_unary(u)?,
            Expression::BinaryExpression(b) => {
                self.compile_expr(&b.left)?;
                self.compile_expr(&b.right)?;
                self.emit0(binary_op(b.operator)?);
            }
            Expression::LogicalExpression(l) => self.compile_logical(l)?,
            Expression::ConditionalExpression(c) => {
                self.compile_expr(&c.test)?;
                let to_else = self.emit_jump("JMP_IF_FALSE");
                self.compile_expr(&c.consequent)?;
                let to_end = self.emit_jump("JMP");
                self.patch_here(to_else);
                self.compile_expr(&c.alternate)?;
                self.patch_here(to_end);
            }
            Expression::AssignmentExpression(a) => self.compile_assign(a)?,
            Expression::UpdateExpression(u) => self.compile_update(u)?,
            Expression::FunctionExpression(func) => {
                let fb = func
                    .body
                    .as_ref()
                    .ok_or(Unsupported("function without a body"))?;
                match &func.id {
                    None => {
                        let idx = self.compile_function(
                            &func.params,
                            FnBody::Block(&fb.statements),
                            None,
                            false,
                            func.r#async,
                            func.generator,
                        )?;
                        self.emit1("MAKE_FUNC", idx as i64);
                    }
                    Some(id) => {
                        // A named function expression binds its own name to the
                        // function, visible only inside the body. A wrapper scope
                        // holds the slot; the closure captures that scope, so the
                        // name resolves there even after the scope leaves the
                        // chain. The outer scope never sees the name.
                        let name = id.name.as_str().to_string();
                        self.open_scope();
                        let slot = self.add_local(&name);
                        self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | 2);
                        let idx = self.compile_function(
                            &func.params,
                            FnBody::Block(&fb.statements),
                            Some(name),
                            false,
                            func.r#async,
                            func.generator,
                        )?;
                        self.emit1("MAKE_FUNC", idx as i64);
                        self.emit0("DUP");
                        self.emit1("INIT_SLOT", pack_addr(0, slot));
                        self.close_scope();
                    }
                }
            }
            Expression::ClassExpression(class) => {
                if let Some(name) = &class.id {
                    self.open_scope();
                    let slot = self.add_local(name.name.as_str());
                    self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | 2);
                    self.compile_class(class, None, Some(pack_addr(0, slot)))?;
                    self.close_scope();
                } else {
                    // A bare anonymous class expression reaches this arm only
                    // outside a named-evaluation position: a declarator, a field,
                    // or a plain identifier assignment routes through
                    // `compile_named_expr` with its inferred name. Outside those,
                    // `(class {}).name` is the empty string by spec, so compiling
                    // with no inferred name matches native whether or not the chunk
                    // observes a callable name.
                    self.compile_class(class, None, None)?;
                }
            }
            Expression::ArrowFunctionExpression(arrow) => {
                let idx = match &arrow.body {
                    ArrowFunctionBody::FunctionBody(fb) => self.compile_function(
                        &arrow.params,
                        FnBody::Block(&fb.statements),
                        None,
                        true,
                        arrow.r#async,
                        false,
                    )?,
                    _ => {
                        let e = arrow.get_expression().ok_or(Unsupported("arrow body"))?;
                        self.compile_function(
                            &arrow.params,
                            FnBody::Expr(e),
                            None,
                            true,
                            arrow.r#async,
                            false,
                        )?
                    }
                };
                self.emit1("MAKE_FUNC", idx as i64);
            }
            Expression::StaticMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional member access"));
                }
                // super.prop reads off the prototype of the method's home
                // object with `this` as the receiver.
                if matches!(&m.object, Expression::Super(_)) {
                    let i = self.consts.add_str(m.property.name.as_str());
                    self.emit1("SUPER_GET", i as i64);
                } else {
                    self.compile_expr(&m.object)?;
                    let i = self.consts.add_str(m.property.name.as_str());
                    self.emit1("GET_PROP", i as i64);
                }
            }
            Expression::ComputedMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional member access"));
                }
                if matches!(&m.object, Expression::Super(_)) {
                    return Err(Unsupported("computed super member access"));
                }
                self.compile_expr(&m.object)?;
                self.compile_expr(&m.expression)?;
                self.emit0("GET_PROP_DYN");
            }
            Expression::PrivateFieldExpression(m) => {
                // `obj.#x`: read the private field through the class private env,
                // which brand-checks the receiver.
                if m.optional {
                    return Err(Unsupported("optional private member access"));
                }
                self.compile_expr(&m.object)?;
                let i = self.consts.add_str(format!("#{}", m.field.name).as_str());
                self.emit1("GET_PRIVATE", i as i64);
            }
            Expression::PrivateInExpression(e) => {
                // `#x in obj`: a brand check, not a property lookup.
                self.compile_expr(&e.right)?;
                let i = self.consts.add_str(format!("#{}", e.left.name).as_str());
                self.emit1("HAS_PRIVATE", i as i64);
            }
            Expression::CallExpression(call) => self.compile_call(call)?,
            Expression::NewExpression(new) => {
                self.compile_expr(&new.callee)?;
                let (argc, spread) = self.compile_args(&new.arguments)?;
                if spread {
                    self.emit0("NEW_APPLY");
                } else {
                    self.emit1("NEW", argc as i64);
                }
            }
            Expression::ArrayExpression(arr) => {
                self.emit0("ARR_EMPTY");
                for el in &arr.elements {
                    match el {
                        ArrayExpressionElement::Elision(_) => {
                            // Increasing an array's length creates a hole.
                            // Pushing undefined would instead create an own
                            // element and change property-existence checks.
                            self.emit0("DUP");
                            self.emit0("DUP");
                            let length = self.consts.add_str("length");
                            self.emit1("GET_PROP", length as i64);
                            let one = self.consts.add_num(1.0);
                            self.emit1("PUSH_CONST", one as i64);
                            self.emit0("ADD");
                            self.emit1("SET_PROP", length as i64);
                            self.emit0("POP");
                        }
                        ArrayExpressionElement::SpreadElement(spread) => {
                            self.compile_expr(&spread.argument)?;
                            self.emit0("ARR_SPREAD");
                        }
                        other => {
                            let e = other.as_expression().ok_or(Unsupported("array element"))?;
                            self.compile_expr(e)?;
                            self.emit0("ARR_APPEND");
                        }
                    }
                }
            }
            Expression::ObjectExpression(obj) => {
                self.emit0("OBJ_EMPTY");
                for prop in &obj.properties {
                    let p = match prop {
                        ObjectPropertyKind::ObjectProperty(p) => p,
                        ObjectPropertyKind::SpreadProperty(spread) => {
                            self.compile_expr(&spread.argument)?;
                            self.emit0("OBJ_SPREAD");
                            continue;
                        }
                    };
                    if matches!(p.kind, PropertyKind::Get | PropertyKind::Set) {
                        // A getter or setter defines an accessor. Two accessors
                        // on the same configurable key merge, so `get`/`set`
                        // pairs compose. A computed key is evaluated and pushed
                        // before the accessor function, then a dynamic define op
                        // reads it off the stack.
                        let static_key = if p.computed { None } else { static_key(&p.key) };
                        if let Some(key) = static_key {
                            self.compile_expr(&p.value)?;
                            let i = self.consts.add_str(&key);
                            let op = if p.kind == PropertyKind::Get {
                                "OBJ_DEFINE_GETTER"
                            } else {
                                "OBJ_DEFINE_SETTER"
                            };
                            self.emit1(op, i as i64);
                        } else {
                            let key = p
                                .key
                                .as_expression()
                                .ok_or(Unsupported("object accessor key expression"))?;
                            self.compile_expr(key)?;
                            self.compile_expr(&p.value)?;
                            let op = if p.kind == PropertyKind::Get {
                                "OBJ_DEFINE_GETTER_DYN"
                            } else {
                                "OBJ_DEFINE_SETTER_DYN"
                            };
                            self.emit0(op);
                        }
                        continue;
                    }
                    // A method shorthand (`{ foo() {} }`) is an Init property
                    // whose value is a function, so it takes the path below.
                    let static_key = if p.computed { None } else { static_key(&p.key) };
                    let computed_key = static_key.is_none()
                        || (p.shorthand && static_key.as_deref() == Some("__proto__"));
                    if computed_key {
                        if p.computed || static_key.is_none() {
                            let key = p
                                .key
                                .as_expression()
                                .ok_or(Unsupported("object property expression"))?;
                            self.compile_expr(key)?;
                        } else {
                            let key = static_key.as_deref().expect("shorthand proto");
                            let i = self.consts.add_str(key);
                            self.emit1("PUSH_CONST", i as i64);
                        }
                    }
                    if computed_key {
                        // A computed key names its anonymous class/function value
                        // at run time, which this path cannot do, so an observed
                        // name declines rather than ship the empty string.
                        if self.keep_names && is_anonymous_class(&p.value) {
                            return Err(Unsupported("anonymous class under a computed key"));
                        }
                        self.compile_expr(&p.value)?;
                        self.emit0("OBJ_SET_DYN");
                    } else {
                        let key = static_key.as_deref().expect("static key");
                        // A static-key data property names an anonymous class or
                        // function value after its key. `__proto__:` sets the
                        // prototype and triggers no named evaluation.
                        if key == "__proto__" {
                            self.compile_expr(&p.value)?;
                        } else {
                            self.compile_named_expr(&p.value, key)?;
                        }
                        let i = self.consts.add_str(key);
                        self.emit1("OBJ_SET", i as i64);
                    }
                }
            }
            Expression::AwaitExpression(a) => {
                self.compile_expr(&a.argument)?;
                // The parser only accepts await inside an async function or a
                // module top level, so the enclosing frame runs async. Raise the
                // flag (already set for a source `async` function; this is what
                // turns a module main with top-level await into a promise) and
                // suspend. AWAIT_TOP is the module-root form; both suspend the
                // same way in the runtime.
                let top = self.fns.last().expect("frame").is_module_main;
                self.fns.last_mut().expect("frame").is_async = true;
                self.emit0(if top { "AWAIT_TOP" } else { "AWAIT" });
            }
            Expression::YieldExpression(y) => {
                // The parser only accepts yield inside a generator, so the frame
                // is a generator. `yield arg` suspends with the value; the driver
                // pushes the value sent to the next `next` call as this
                // expression's result. `yield* arg` pushes the iterable and
                // delegates: the driver drives its iterator, forwarding
                // next/throw/return, and leaves the delegate's return value here.
                // `yield*` in an async generator delegates to an async iterator,
                // which the async-generator driver does not drive yet; decline.
                if y.delegate && self.fns.last().expect("frame").is_async {
                    return Err(Unsupported("yield* in an async generator"));
                }
                match &y.argument {
                    Some(arg) => self.compile_expr(arg)?,
                    None => self.emit0("PUSH_UNDEF"),
                }
                self.emit0(if y.delegate { "YIELD_STAR" } else { "YIELD" });
            }
            Expression::TaggedTemplateExpression(tagged) => self.compile_tagged_template(tagged)?,
            Expression::ImportMeta(_) => {
                // import.meta is read from the native module wrapper.
                self.uses_import_meta = true;
                self.emit0("IMPORT_META");
            }
            Expression::NewTarget(_) => {
                self.emit0("LOAD_NEW_TARGET");
            }
            Expression::ImportExpression(i) => {
                // Dynamic import() calls the native import the wrapper supplies.
                // Import options and phases are not forwarded yet.
                if i.options.is_some() || i.phase.is_some() {
                    return Err(Unsupported("dynamic import options or phase"));
                }
                self.compile_expr(&i.source)?;
                self.uses_dynamic_import = true;
                self.emit0("DYNAMIC_IMPORT");
            }
            _ => return Err(Unsupported("expression outside the subset")),
        }
        Ok(())
    }

    fn compile_unary(&mut self, u: &oxc_ast::ast::UnaryExpression<'_>) -> R<()> {
        match u.operator {
            UnaryOperator::Typeof => {
                if let Expression::Identifier(id) = &u.argument {
                    self.emit_typeof_name(id.name.as_str())?;
                } else {
                    self.compile_expr(&u.argument)?;
                    self.emit0("TYPEOF");
                }
            }
            UnaryOperator::UnaryNegation => {
                self.compile_expr(&u.argument)?;
                self.emit0("NEG");
            }
            UnaryOperator::UnaryPlus => {
                self.compile_expr(&u.argument)?;
                self.emit0("POS");
            }
            UnaryOperator::LogicalNot => {
                self.compile_expr(&u.argument)?;
                self.emit0("NOT");
            }
            UnaryOperator::BitwiseNot => {
                self.compile_expr(&u.argument)?;
                self.emit0("BNOT");
            }
            UnaryOperator::Void => {
                self.compile_expr(&u.argument)?;
                self.emit0("POP");
                self.emit0("PUSH_UNDEF");
            }
            UnaryOperator::Delete => match &u.argument {
                Expression::StaticMemberExpression(m) if !m.optional => {
                    self.compile_expr(&m.object)?;
                    let i = self.consts.add_str(m.property.name.as_str());
                    self.emit1("DELETE_PROP", i as i64);
                }
                Expression::ComputedMemberExpression(m) if !m.optional => {
                    self.compile_expr(&m.object)?;
                    self.compile_expr(&m.expression)?;
                    self.emit0("DELETE_PROP_DYN");
                }
                // `delete x` on a plain binding is a strict-mode syntax error,
                // and an optional or private member delete is out of the subset.
                _ => return Err(Unsupported("delete of this target")),
            },
        }
        Ok(())
    }

    /// Compile `obj.key <op>= rhs` and `obj[key] <op>= rhs` (plain compound) by
    /// evaluating the object (and computed key) once, reading the current value,
    /// combining, and writing back. The result is the stored value.
    fn compile_member_compound(
        &mut self,
        me: &MemberExpression<'_>,
        bin: &'static str,
        rhs: &Expression<'_>,
    ) -> R<()> {
        match me {
            MemberExpression::StaticMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional assignment target"));
                }
                let key = self.consts.add_str(m.property.name.as_str());
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                self.emit1("GET_PROP", key as i64);
                self.compile_expr(rhs)?;
                self.emit0(bin);
                self.emit1("SET_PROP", key as i64);
            }
            MemberExpression::ComputedMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional assignment target"));
                }
                let t_o = self.declare_temp();
                let t_k = self.declare_temp();
                let t_v = self.declare_temp();
                self.compile_expr(&m.object)?;
                self.emit1("INIT_SLOT", pack_addr(0, t_o));
                self.compile_expr(&m.expression)?;
                self.emit1("INIT_SLOT", pack_addr(0, t_k));
                self.emit1("LOAD_SLOT", pack_addr(0, t_o));
                self.emit1("LOAD_SLOT", pack_addr(0, t_k));
                self.emit0("GET_PROP_DYN");
                self.compile_expr(rhs)?;
                self.emit0(bin);
                self.emit1("INIT_SLOT", pack_addr(0, t_v));
                self.emit1("LOAD_SLOT", pack_addr(0, t_o));
                self.emit1("LOAD_SLOT", pack_addr(0, t_k));
                self.emit1("LOAD_SLOT", pack_addr(0, t_v));
                self.emit0("SET_PROP_DYN");
            }
            MemberExpression::PrivateFieldExpression(_) => {
                return Err(Unsupported("private field assignment"))
            }
        }
        Ok(())
    }

    /// Compile `obj.key <op>= rhs` for the logical operators (`&&=`, `||=`,
    /// `??=`), which read the current value and assign `rhs` only when the
    /// short-circuit condition fails. `keep_jump` is the KEEP jump that skips the
    /// assignment: truthy for `||=`, falsy for `&&=`, non-nullish for `??=`. The
    /// object (and computed key) is evaluated once; `rhs` only when it runs.
    fn compile_member_logical(
        &mut self,
        me: &MemberExpression<'_>,
        keep_jump: &'static str,
        rhs: &Expression<'_>,
    ) -> R<()> {
        match me {
            MemberExpression::StaticMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional assignment target"));
                }
                let key = self.consts.add_str(m.property.name.as_str());
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                self.emit1("GET_PROP", key as i64);
                let keep = self.emit_jump(keep_jump);
                self.compile_expr(rhs)?;
                self.emit1("SET_PROP", key as i64);
                let to_end = self.emit_jump("JMP");
                self.patch_here(keep);
                self.emit0("SWAP");
                self.emit0("POP");
                self.patch_here(to_end);
            }
            MemberExpression::ComputedMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional assignment target"));
                }
                let t_o = self.declare_temp();
                let t_k = self.declare_temp();
                self.compile_expr(&m.object)?;
                self.emit1("INIT_SLOT", pack_addr(0, t_o));
                self.compile_expr(&m.expression)?;
                self.emit1("INIT_SLOT", pack_addr(0, t_k));
                self.emit1("LOAD_SLOT", pack_addr(0, t_o));
                self.emit1("LOAD_SLOT", pack_addr(0, t_k));
                self.emit0("GET_PROP_DYN");
                let keep = self.emit_jump(keep_jump);
                let t_v = self.declare_temp();
                self.compile_expr(rhs)?;
                self.emit1("INIT_SLOT", pack_addr(0, t_v));
                self.emit1("LOAD_SLOT", pack_addr(0, t_o));
                self.emit1("LOAD_SLOT", pack_addr(0, t_k));
                self.emit1("LOAD_SLOT", pack_addr(0, t_v));
                self.emit0("SET_PROP_DYN");
                let to_end = self.emit_jump("JMP");
                self.patch_here(keep);
                self.patch_here(to_end);
            }
            MemberExpression::PrivateFieldExpression(_) => {
                return Err(Unsupported("private field assignment"))
            }
        }
        Ok(())
    }

    fn compile_logical(&mut self, l: &oxc_ast::ast::LogicalExpression<'_>) -> R<()> {
        self.compile_expr(&l.left)?;
        let short = match l.operator {
            LogicalOperator::And => "JMP_IF_FALSY_KEEP",
            LogicalOperator::Or => "JMP_IF_TRUTHY_KEEP",
            LogicalOperator::Coalesce => "JMP_IF_NOT_NULLISH_KEEP",
        };
        let to_end = self.emit_jump(short);
        self.compile_expr(&l.right)?;
        self.patch_here(to_end);
        Ok(())
    }

    fn compile_assign(&mut self, node: &AssignmentExpression<'_>) -> R<()> {
        match node.operator {
            AssignmentOperator::Assign => match &node.left {
                AssignmentTarget::AssignmentTargetIdentifier(id) => {
                    // Assigning an anonymous class, function, or arrow to a plain
                    // identifier infers the binding's name, the same as a
                    // declarator does, so route it through the named path.
                    self.compile_named_expr(&node.right, id.name.as_str())?;
                    self.emit_store_name(id.name.as_str());
                }
                AssignmentTarget::ArrayAssignmentTarget(_)
                | AssignmentTarget::ObjectAssignmentTarget(_) => {
                    // The assignment value is the right side; keep a copy as the
                    // expression result and destructure the duplicate.
                    self.compile_expr(&node.right)?;
                    self.emit0("DUP");
                    self.destructure_assign(&node.left)?;
                }
                other => {
                    let me = other
                        .as_member_expression()
                        .ok_or(Unsupported("assignment target"))?;
                    match me {
                        MemberExpression::StaticMemberExpression(m) => {
                            if m.optional {
                                return Err(Unsupported("optional assignment target"));
                            }
                            self.compile_expr(&m.object)?;
                            self.compile_expr(&node.right)?;
                            let i = self.consts.add_str(m.property.name.as_str());
                            self.emit1("SET_PROP", i as i64);
                        }
                        MemberExpression::ComputedMemberExpression(m) => {
                            if m.optional {
                                return Err(Unsupported("optional assignment target"));
                            }
                            self.compile_expr(&m.object)?;
                            self.compile_expr(&m.expression)?;
                            self.compile_expr(&node.right)?;
                            self.emit0("SET_PROP_DYN");
                        }
                        MemberExpression::PrivateFieldExpression(m) => {
                            // `obj.#x = v`: write through the class private env,
                            // which brand-checks the receiver.
                            self.compile_expr(&m.object)?;
                            self.compile_expr(&node.right)?;
                            let i = self.consts.add_str(format!("#{}", m.field.name).as_str());
                            self.emit1("SET_PRIVATE", i as i64);
                        }
                    }
                }
            },
            AssignmentOperator::LogicalAnd
            | AssignmentOperator::LogicalOr
            | AssignmentOperator::LogicalNullish => {
                let short = match node.operator {
                    AssignmentOperator::LogicalAnd => "JMP_IF_FALSY_KEEP",
                    AssignmentOperator::LogicalOr => "JMP_IF_TRUTHY_KEEP",
                    _ => "JMP_IF_NOT_NULLISH_KEEP",
                };
                match &node.left {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => {
                        self.emit_load_name(id.name.as_str())?;
                        let to_end = self.emit_jump(short);
                        self.emit0("POP");
                        self.compile_expr(&node.right)?;
                        self.emit_store_name(id.name.as_str());
                        self.patch_here(to_end);
                    }
                    left => {
                        let me = left
                            .as_member_expression()
                            .ok_or(Unsupported("logical assignment to this target"))?;
                        self.compile_member_logical(me, short, &node.right)?;
                    }
                }
            }
            other => {
                let bin = compound_binary_op(other)?;
                match &node.left {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => {
                        self.emit_load_name(id.name.as_str())?;
                        self.compile_expr(&node.right)?;
                        self.emit0(bin);
                        self.emit_store_name(id.name.as_str());
                    }
                    left => {
                        let me = left
                            .as_member_expression()
                            .ok_or(Unsupported("compound assignment to this target"))?;
                        self.compile_member_compound(me, bin, &node.right)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn compile_update(&mut self, node: &oxc_ast::ast::UpdateExpression<'_>) -> R<()> {
        let delta = match node.operator {
            UpdateOperator::Increment => 1.0,
            UpdateOperator::Decrement => -1.0,
        };
        let id = match &node.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => id,
            other => {
                let me = other
                    .as_member_expression()
                    .ok_or(Unsupported("update on this target"))?;
                return self.compile_member_update(me, delta, node.prefix);
            }
        };
        let name = id.name.as_str().to_string();
        // Mirror the shield: coerce to a number with POS, then add +/-1. This is
        // correct for number and string operands; a BigInt operand differs (as in
        // the shield), which is the one documented limitation.
        self.emit_load_name(&name)?;
        self.emit0("POS");
        if node.prefix {
            let i = self.consts.add_num(delta);
            self.emit1("PUSH_CONST", i as i64);
            self.emit0("ADD");
            self.emit_store_name(&name);
        } else {
            self.emit0("DUP");
            let i = self.consts.add_num(delta);
            self.emit1("PUSH_CONST", i as i64);
            self.emit0("ADD");
            self.emit_store_name(&name);
            self.emit0("POP");
        }
        Ok(())
    }

    /// Compile `obj.key++`, `--obj[key]`, and friends. The object (and computed
    /// key) is evaluated once. Like the identifier form, the operand is coerced
    /// with `POS` before adding +/-1, so a number or string operand is correct
    /// and a BigInt operand differs (the same documented limitation). A prefix
    /// update yields the new value, a postfix update the old.
    fn compile_member_update(
        &mut self,
        me: &MemberExpression<'_>,
        delta: f64,
        prefix: bool,
    ) -> R<()> {
        match me {
            MemberExpression::StaticMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional update target"));
                }
                let key = self.consts.add_str(m.property.name.as_str());
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                self.emit1("GET_PROP", key as i64);
                self.emit0("POS");
                if prefix {
                    let i = self.consts.add_num(delta);
                    self.emit1("PUSH_CONST", i as i64);
                    self.emit0("ADD");
                    self.emit1("SET_PROP", key as i64);
                } else {
                    let t_old = self.declare_temp();
                    self.emit1("INIT_SLOT", pack_addr(0, t_old));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_old));
                    let i = self.consts.add_num(delta);
                    self.emit1("PUSH_CONST", i as i64);
                    self.emit0("ADD");
                    self.emit1("SET_PROP", key as i64);
                    self.emit0("POP");
                    self.emit1("LOAD_SLOT", pack_addr(0, t_old));
                }
            }
            MemberExpression::ComputedMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional update target"));
                }
                let t_o = self.declare_temp();
                let t_k = self.declare_temp();
                self.compile_expr(&m.object)?;
                self.emit1("INIT_SLOT", pack_addr(0, t_o));
                self.compile_expr(&m.expression)?;
                self.emit1("INIT_SLOT", pack_addr(0, t_k));
                self.emit1("LOAD_SLOT", pack_addr(0, t_o));
                self.emit1("LOAD_SLOT", pack_addr(0, t_k));
                self.emit0("GET_PROP_DYN");
                self.emit0("POS");
                let old = self.consts.add_num(delta);
                if prefix {
                    self.emit1("PUSH_CONST", old as i64);
                    self.emit0("ADD");
                    let t_v = self.declare_temp();
                    self.emit1("INIT_SLOT", pack_addr(0, t_v));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_o));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_k));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_v));
                    self.emit0("SET_PROP_DYN");
                } else {
                    let t_old = self.declare_temp();
                    self.emit1("INIT_SLOT", pack_addr(0, t_old));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_old));
                    self.emit1("PUSH_CONST", old as i64);
                    self.emit0("ADD");
                    let t_v = self.declare_temp();
                    self.emit1("INIT_SLOT", pack_addr(0, t_v));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_o));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_k));
                    self.emit1("LOAD_SLOT", pack_addr(0, t_v));
                    self.emit0("SET_PROP_DYN");
                    self.emit0("POP");
                    self.emit1("LOAD_SLOT", pack_addr(0, t_old));
                }
            }
            MemberExpression::PrivateFieldExpression(_) => {
                return Err(Unsupported("private field update"))
            }
        }
        Ok(())
    }

    fn compile_call(&mut self, call: &oxc_ast::ast::CallExpression<'_>) -> R<()> {
        if call.optional {
            return Err(Unsupported("optional call"));
        }
        // Parentheses preserve the receiver of a method reference. A comma
        // expression drops the receiver, so leave that expression intact.
        let mut callee = &call.callee;
        while let Expression::ParenthesizedExpression(parenthesized) = callee {
            callee = &parenthesized.expression;
        }
        if matches!(callee, Expression::ChainExpression(_)) {
            return Err(Unsupported("call of parenthesized optional chain"));
        }
        // Direct eval observes its caller's lexical bindings. An indirect
        // global call from the VM would give a different environment.
        if let Expression::Identifier(id) = callee {
            if id.name == "eval" {
                return Err(Unsupported("direct eval"));
            }
        }
        match callee {
            // super(...args): construct the superclass and bind `this`.
            Expression::Super(_) => {
                let (argc, spread) = self.compile_args(&call.arguments)?;
                if spread {
                    // The arguments were gathered into one array; construct the
                    // superclass by applying it, the same as a spread call or new.
                    self.emit0("SUPER_CALL_APPLY");
                } else {
                    self.emit1("SUPER_CALL", argc as i64);
                }
            }
            // super.method(args): the receiver is `this`, but the method comes
            // from the prototype of the method's home object.
            Expression::StaticMemberExpression(m)
                if !m.optional && matches!(&m.object, Expression::Super(_)) =>
            {
                self.emit0("PUSH_THIS");
                let i = self.consts.add_str(m.property.name.as_str());
                self.emit1("SUPER_GET", i as i64);
                let (argc, spread) = self.compile_args(&call.arguments)?;
                if spread {
                    return Err(Unsupported("spread in a super method call"));
                }
                self.emit1("CALL_METHOD", argc as i64);
            }
            Expression::StaticMemberExpression(m) if !m.optional => {
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                let i = self.consts.add_str(m.property.name.as_str());
                self.emit1("GET_PROP", i as i64);
                let (argc, spread) = self.compile_args(&call.arguments)?;
                if spread {
                    self.emit0("CALL_METHOD_APPLY");
                } else {
                    self.emit1("CALL_METHOD", argc as i64);
                }
            }
            Expression::ComputedMemberExpression(m)
                if !m.optional && matches!(&m.object, Expression::Super(_)) =>
            {
                return Err(Unsupported("computed super member call"));
            }
            Expression::ComputedMemberExpression(m) if !m.optional => {
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                self.compile_expr(&m.expression)?;
                self.emit0("GET_PROP_DYN");
                let (argc, spread) = self.compile_args(&call.arguments)?;
                if spread {
                    self.emit0("CALL_METHOD_APPLY");
                } else {
                    self.emit1("CALL_METHOD", argc as i64);
                }
            }
            // `obj.#x(args)`: the receiver is kept so a private field holding a
            // regular function sees the right `this`.
            Expression::PrivateFieldExpression(m) if !m.optional => {
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                let i = self.consts.add_str(format!("#{}", m.field.name).as_str());
                self.emit1("GET_PRIVATE", i as i64);
                let (argc, spread) = self.compile_args(&call.arguments)?;
                if spread {
                    self.emit0("CALL_METHOD_APPLY");
                } else {
                    self.emit1("CALL_METHOD", argc as i64);
                }
            }
            callee => {
                self.compile_expr(callee)?;
                let (argc, spread) = self.compile_args(&call.arguments)?;
                if spread {
                    self.emit0("CALL_APPLY");
                } else {
                    self.emit1("CALL", argc as i64);
                }
            }
        }
        Ok(())
    }

    /// `tag`...`` calls `tag(strings, ...substitutions)`. The strings object is a
    /// frozen const built once per site (see `ConstPool::add_template`), so the
    /// call site always passes the same object. A member tag keeps its receiver
    /// so `this` is the object, matching a normal method call.
    fn compile_tagged_template(
        &mut self,
        tagged: &oxc_ast::ast::TaggedTemplateExpression<'_>,
    ) -> R<()> {
        let mut tag = &tagged.tag;
        while let Expression::ParenthesizedExpression(p) = tag {
            tag = &p.expression;
        }
        if matches!(tag, Expression::ChainExpression(_)) {
            return Err(Unsupported("tagged template on an optional chain"));
        }
        let quasi = &tagged.quasi;
        let cooked: Vec<Option<String>> = quasi
            .quasis
            .iter()
            .map(|q| {
                q.value
                    .cooked
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        let raw: Vec<String> = quasi
            .quasis
            .iter()
            .map(|q| q.value.raw.as_str().to_string())
            .collect();
        let strings = self.consts.add_template(&cooked, &raw);
        let is_method = match tag {
            Expression::StaticMemberExpression(m) if !m.optional => {
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                let i = self.consts.add_str(m.property.name.as_str());
                self.emit1("GET_PROP", i as i64);
                true
            }
            Expression::ComputedMemberExpression(m) if !m.optional => {
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                self.compile_expr(&m.expression)?;
                self.emit0("GET_PROP_DYN");
                true
            }
            other => {
                self.compile_expr(other)?;
                false
            }
        };
        self.emit1("PUSH_CONST", strings as i64);
        for expr in &quasi.expressions {
            self.compile_expr(expr)?;
        }
        let argc = (quasi.expressions.len() + 1) as i64;
        if is_method {
            self.emit1("CALL_METHOD", argc);
        } else {
            self.emit1("CALL", argc);
        }
        Ok(())
    }

    // A chain maintains [receiver, value] as its stack pair. The receiver is
    // preserved across a method lookup and consumed only when a call occurs.
    // Every optional step jumps to the same final result on nullish input, so
    // subsequent computed keys and arguments are never evaluated.
    fn compile_chain(&mut self, chain: &ChainExpression<'_>) -> R<()> {
        let mut early_exits = Vec::new();
        match &chain.expression {
            ChainElement::StaticMemberExpression(member) => {
                self.compile_chain_static(member, &mut early_exits)?;
            }
            ChainElement::ComputedMemberExpression(member) => {
                self.compile_chain_computed(member, &mut early_exits)?;
            }
            ChainElement::CallExpression(call) => {
                self.compile_chain_call(call, &mut early_exits)?;
            }
            _ => return Err(Unsupported("unsupported chain element")),
        }
        self.emit0("SWAP");
        self.emit0("POP");
        for exit in early_exits {
            self.patch_here(exit);
        }
        Ok(())
    }

    fn compile_chain_inner(&mut self, expr: &Expression<'_>, exits: &mut Vec<usize>) -> R<()> {
        match expr {
            Expression::StaticMemberExpression(member) => self.compile_chain_static(member, exits),
            Expression::ComputedMemberExpression(member) => {
                self.compile_chain_computed(member, exits)
            }
            Expression::CallExpression(call) => self.compile_chain_call(call, exits),
            base => {
                self.emit0("PUSH_UNDEF");
                self.compile_expr(base)
            }
        }
    }

    fn optional_chain_guard(&mut self, exits: &mut Vec<usize>) {
        self.emit0("DUP");
        let present = self.emit_jump("JMP_IF_NOT_NULLISH_KEEP");
        // On nullish input the jump opcode discarded the duplicate. Discard
        // the receiver and value, then push the chain's undefined result.
        self.emit0("POP");
        self.emit0("POP");
        self.emit0("PUSH_UNDEF");
        exits.push(self.emit_jump("JMP"));
        self.patch_here(present);
        self.emit0("POP");
    }

    fn compile_chain_static(
        &mut self,
        member: &StaticMemberExpression<'_>,
        exits: &mut Vec<usize>,
    ) -> R<()> {
        self.compile_chain_inner(&member.object, exits)?;
        if member.optional {
            self.optional_chain_guard(exits);
        }
        self.emit0("SWAP");
        self.emit0("POP");
        self.emit0("DUP");
        let index = self.consts.add_str(member.property.name.as_str());
        self.emit1("GET_PROP", index as i64);
        Ok(())
    }

    fn compile_chain_computed(
        &mut self,
        member: &ComputedMemberExpression<'_>,
        exits: &mut Vec<usize>,
    ) -> R<()> {
        self.compile_chain_inner(&member.object, exits)?;
        if member.optional {
            self.optional_chain_guard(exits);
        }
        self.emit0("SWAP");
        self.emit0("POP");
        self.emit0("DUP");
        self.compile_expr(&member.expression)?;
        self.emit0("GET_PROP_DYN");
        Ok(())
    }

    fn compile_chain_call(
        &mut self,
        call: &oxc_ast::ast::CallExpression<'_>,
        exits: &mut Vec<usize>,
    ) -> R<()> {
        self.compile_chain_inner(&call.callee, exits)?;
        if call.optional {
            self.optional_chain_guard(exits);
        }
        let (count, spread) = self.compile_args(&call.arguments)?;
        if spread {
            self.emit0("CALL_METHOD_APPLY");
        } else {
            self.emit1("CALL_METHOD", count as i64);
        }
        self.emit0("PUSH_UNDEF");
        self.emit0("SWAP");
        Ok(())
    }

    fn compile_args(&mut self, args: &[Argument<'_>]) -> R<(usize, bool)> {
        let spread = args
            .iter()
            .any(|arg| matches!(arg, Argument::SpreadElement(_)));
        if spread {
            self.emit0("ARR_EMPTY");
        }
        for arg in args {
            match arg {
                Argument::SpreadElement(element) => {
                    self.compile_expr(&element.argument)?;
                    self.emit0("ARR_SPREAD");
                }
                other => {
                    let expression = other.as_expression().ok_or(Unsupported("call argument"))?;
                    self.compile_expr(expression)?;
                    if spread {
                        self.emit0("ARR_APPEND");
                    }
                }
            }
        }
        Ok((args.len(), spread))
    }

    // ---- name resolution ----

    fn root_class_needs_module_mode(&self) -> bool {
        let frame = self.fns.last().expect("frame");
        frame.is_module_main
            && !frame.allow_root_class
            && self.scopes.len() - 1 == frame.base_scope_index
    }

    fn add_local(&mut self, name: &str) -> usize {
        let scope = self.scopes.last_mut().expect("a scope is open");
        let idx = scope.names.len();
        scope.names.push(name.to_string());
        idx
    }

    /// Reserve a throwaway slot in the current scope for an intermediate value,
    /// declared as a plain (var-kind) cell so it can be initialized and read
    /// freely. Used to hold the object, key, and new value while rewriting a
    /// member compound assignment or update, so each is evaluated once.
    fn declare_temp(&mut self) -> usize {
        let slot = self.add_local("*tmp*");
        self.emit1("DECLARE_SLOT", (slot as i64) << 2);
        slot
    }

    /// Store the destructured value on top of the stack into a binding leaf,
    /// consuming it. A lexical leaf initializes its TDZ cell; a `var` leaf
    /// stores and drops the duplicate the store leaves behind.
    fn bind_leaf(&mut self, name: &str, mode: BindMode) {
        match mode {
            BindMode::Lexical => self.emit_init_name(name),
            BindMode::Var => {
                self.emit_store_name(name);
                self.emit0("POP");
            }
        }
    }

    fn emit_init_name(&mut self, name: &str) {
        let (depth, slot) = self
            .resolve(name)
            .expect("a destructuring leaf initializes a declared slot");
        self.emit1("INIT_SLOT", pack_addr(depth, slot));
    }

    /// Value on top of the stack: if it is `undefined`, replace it with the
    /// default. This is the per-leaf default of `{ a = 1 }` and `[a = 2]`, which
    /// applies only to `undefined`, not to `null` or other falsy values.
    fn apply_default(&mut self, default: &Expression<'_>) -> R<()> {
        // A default value names an anonymous class after its binding, which this
        // path does not know here, so an observed name declines rather than ship
        // the empty string. A dropped-name chunk compiles it with no name.
        if self.keep_names && is_anonymous_class(default) {
            return Err(Unsupported("anonymous class as a default value"));
        }
        self.emit0("DUP");
        self.emit0("PUSH_UNDEF");
        self.emit0("SEQ");
        let have = self.emit_jump("JMP_IF_FALSE");
        self.emit0("POP");
        self.compile_expr(default)?;
        self.patch_here(have);
        Ok(())
    }

    /// Destructure the value on top of the stack into a binding pattern,
    /// consuming it. Leaves bind through [`bind_leaf`]. Array patterns walk the
    /// iterator protocol; object patterns read named properties and copy the
    /// remainder for `...rest`.
    fn destructure_binding(&mut self, pat: &BindingPattern<'_>, mode: BindMode) -> R<()> {
        match pat {
            BindingPattern::BindingIdentifier(id) => {
                self.bind_leaf(id.name.as_str(), mode);
            }
            BindingPattern::AssignmentPattern(ap) => {
                self.apply_default(&ap.right)?;
                self.destructure_binding(&ap.left, mode)?;
            }
            BindingPattern::ArrayPattern(arr) => {
                self.emit0("GET_ITERATOR");
                for element in &arr.elements {
                    match element {
                        None => {
                            self.emit0("ITER_NEXT_PUSH");
                            self.emit0("POP");
                        }
                        Some(target) => {
                            self.emit0("ITER_NEXT_PUSH");
                            self.destructure_binding(target, mode)?;
                        }
                    }
                }
                if let Some(rest) = &arr.rest {
                    self.emit0("ITER_REST_PUSH");
                    self.destructure_binding(&rest.argument, mode)?;
                }
                self.emit0("POP");
            }
            BindingPattern::ObjectPattern(obj) => {
                let has_computed = obj.properties.iter().any(|prop| prop.computed);
                if obj.rest.is_some() && has_computed {
                    return self.destructure_object_rest_dynamic(obj, mode);
                }
                let mut excluded: Vec<String> = Vec::new();
                for prop in &obj.properties {
                    self.emit0("DUP");
                    if prop.computed {
                        let key = prop
                            .key
                            .as_expression()
                            .ok_or(Unsupported("object pattern computed key"))?;
                        self.compile_expr(key)?;
                        self.emit0("GET_PROP_DYN");
                    } else {
                        let key = static_key(&prop.key).ok_or(Unsupported("object pattern key"))?;
                        let index = self.consts.add_str(&key);
                        excluded.push(key);
                        self.emit1("GET_PROP", index as i64);
                    }
                    self.destructure_binding(&prop.value, mode)?;
                }
                if let Some(rest) = &obj.rest {
                    let json = serde_json::to_string(&excluded).expect("string list serializes");
                    let index = self.consts.add_str(&json);
                    self.emit1("OBJ_REST", index as i64);
                    self.destructure_binding(&rest.argument, mode)?;
                } else {
                    self.emit0("POP");
                }
            }
        }
        Ok(())
    }

    /// Object pattern with `...rest` after a computed key: the excluded-key set
    /// is not known until the computed keys evaluate, so it is built in a runtime
    /// array and `OBJ_REST_DYN` reads it.
    fn destructure_object_rest_dynamic(
        &mut self,
        obj: &oxc_ast::ast::ObjectPattern<'_>,
        mode: BindMode,
    ) -> R<()> {
        let excluded = self.declare_temp();
        self.emit0("ARR_EMPTY");
        self.emit1("INIT_SLOT", pack_addr(0, excluded));
        for prop in &obj.properties {
            let key_tmp = self.declare_temp();
            if prop.computed {
                let key = prop
                    .key
                    .as_expression()
                    .ok_or(Unsupported("object pattern computed key"))?;
                self.compile_expr(key)?;
            } else {
                let key = static_key(&prop.key).ok_or(Unsupported("object pattern key"))?;
                let index = self.consts.add_str(&key);
                self.emit1("PUSH_CONST", index as i64);
            }
            self.emit1("INIT_SLOT", pack_addr(0, key_tmp));
            self.emit1("LOAD_SLOT", pack_addr(0, excluded));
            self.emit1("LOAD_SLOT", pack_addr(0, key_tmp));
            self.emit0("ARR_APPEND");
            self.emit0("POP");
            self.emit0("DUP");
            self.emit1("LOAD_SLOT", pack_addr(0, key_tmp));
            self.emit0("GET_PROP_DYN");
            self.destructure_binding(&prop.value, mode)?;
        }
        let rest = obj.rest.as_ref().expect("dynamic rest path has a rest");
        self.emit1("LOAD_SLOT", pack_addr(0, excluded));
        self.emit0("OBJ_REST_DYN");
        self.destructure_binding(&rest.argument, mode)?;
        Ok(())
    }

    /// Destructure the value on top of the stack into an assignment target
    /// pattern (`[a, b] = v`, `({ a } = v)`), consuming it. Leaves store to an
    /// existing identifier or member, mirroring [`destructure_binding`] but
    /// targeting places rather than fresh bindings.
    fn destructure_assign(&mut self, target: &AssignmentTarget<'_>) -> R<()> {
        match target {
            AssignmentTarget::ArrayAssignmentTarget(arr) => {
                self.emit0("GET_ITERATOR");
                for element in &arr.elements {
                    match element {
                        None => {
                            self.emit0("ITER_NEXT_PUSH");
                            self.emit0("POP");
                        }
                        Some(target) => {
                            self.emit0("ITER_NEXT_PUSH");
                            self.assign_maybe_default(target)?;
                        }
                    }
                }
                if let Some(rest) = &arr.rest {
                    self.emit0("ITER_REST_PUSH");
                    self.assign_leaf(&rest.target)?;
                }
                self.emit0("POP");
            }
            AssignmentTarget::ObjectAssignmentTarget(obj) => {
                let mut excluded: Vec<String> = Vec::new();
                for prop in &obj.properties {
                    match prop {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(id) => {
                            let name = id.binding.name.as_str().to_string();
                            self.emit0("DUP");
                            let index = self.consts.add_str(&name);
                            self.emit1("GET_PROP", index as i64);
                            if let Some(init) = &id.init {
                                self.apply_default(init)?;
                            }
                            self.emit_store_name(&name);
                            self.emit0("POP");
                            excluded.push(name);
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(pp) => {
                            self.emit0("DUP");
                            if pp.computed {
                                let key = pp
                                    .name
                                    .as_expression()
                                    .ok_or(Unsupported("object assign computed key"))?;
                                self.compile_expr(key)?;
                                self.emit0("GET_PROP_DYN");
                            } else {
                                let key =
                                    static_key(&pp.name).ok_or(Unsupported("object assign key"))?;
                                let index = self.consts.add_str(&key);
                                excluded.push(key);
                                self.emit1("GET_PROP", index as i64);
                            }
                            self.assign_maybe_default(&pp.binding)?;
                        }
                    }
                }
                if let Some(rest) = &obj.rest {
                    let json = serde_json::to_string(&excluded).expect("string list serializes");
                    let index = self.consts.add_str(&json);
                    self.emit1("OBJ_REST", index as i64);
                    self.assign_leaf(&rest.target)?;
                } else {
                    self.emit0("POP");
                }
            }
            simple => self.assign_leaf(simple)?,
        }
        Ok(())
    }

    /// One array or object element target, which may carry a default and may
    /// itself be a nested pattern. Value on top of the stack, consumed.
    fn assign_maybe_default(&mut self, mb: &AssignmentTargetMaybeDefault<'_>) -> R<()> {
        match mb {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(wd) => {
                self.apply_default(&wd.init)?;
                self.assign_leaf(&wd.binding)
            }
            other => {
                let target = other
                    .as_assignment_target()
                    .ok_or(Unsupported("assignment target element"))?;
                self.assign_leaf(target)
            }
        }
    }

    /// Store the value on top of the stack into an assignment target, consuming
    /// it. A nested pattern recurses; an identifier stores by name; a member
    /// stashes the value, rebuilds the reference, and sets the property.
    fn assign_leaf(&mut self, target: &AssignmentTarget<'_>) -> R<()> {
        match target {
            AssignmentTarget::ArrayAssignmentTarget(_)
            | AssignmentTarget::ObjectAssignmentTarget(_) => self.destructure_assign(target),
            AssignmentTarget::AssignmentTargetIdentifier(id) => {
                self.emit_store_name(id.name.as_str());
                self.emit0("POP");
                Ok(())
            }
            member => {
                let me = member
                    .as_member_expression()
                    .ok_or(Unsupported("destructuring assign target"))?;
                let tmp = self.declare_temp();
                self.emit1("INIT_SLOT", pack_addr(0, tmp));
                match me {
                    MemberExpression::StaticMemberExpression(m) => {
                        if m.optional {
                            return Err(Unsupported("optional assignment target"));
                        }
                        self.compile_expr(&m.object)?;
                        self.emit1("LOAD_SLOT", pack_addr(0, tmp));
                        let index = self.consts.add_str(m.property.name.as_str());
                        self.emit1("SET_PROP", index as i64);
                    }
                    MemberExpression::ComputedMemberExpression(m) => {
                        if m.optional {
                            return Err(Unsupported("optional assignment target"));
                        }
                        self.compile_expr(&m.object)?;
                        self.compile_expr(&m.expression)?;
                        self.emit1("LOAD_SLOT", pack_addr(0, tmp));
                        self.emit0("SET_PROP_DYN");
                    }
                    MemberExpression::PrivateFieldExpression(_) => {
                        return Err(Unsupported("private field assignment"));
                    }
                }
                self.emit0("POP");
                Ok(())
            }
        }
    }

    fn compile_var_init(&mut self, name: &str, init: &Expression<'_>) -> R<()> {
        let frame = self.fns.last().expect("frame");
        let slot = frame
            .var_slots
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, slot)| *slot)
            .ok_or(Unsupported("var not hoisted in the function"))?;
        let depth = self.scopes.len() - 1 - frame.base_scope_index;
        self.compile_named_expr(init, name)?;
        self.emit1("STORE_SLOT", pack_addr(depth, slot));
        self.emit0("POP");
        Ok(())
    }

    /// Resolve a name to a packed slot address `(depth, index)`, or `None` for a
    /// free name (a global). `depth` is the number of scope hops up the chain.
    fn resolve(&self, name: &str) -> Option<(usize, usize)> {
        let top = self.scopes.len();
        for (i, scope) in self.scopes.iter().enumerate().rev() {
            if let Some(pos) = scope.names.iter().position(|n| n == name) {
                return Some((top - 1 - i, pos));
            }
        }
        None
    }

    fn emit_load_name(&mut self, name: &str) -> R<()> {
        match self.resolve(name) {
            Some((depth, slot)) => self.emit1("LOAD_SLOT", pack_addr(depth, slot)),
            None => {
                let i = self.consts.add_str(name);
                self.emit1("LOAD_GLOBAL", i as i64);
            }
        }
        Ok(())
    }

    fn emit_store_name(&mut self, name: &str) {
        match self.resolve(name) {
            Some((depth, slot)) => self.emit1("STORE_SLOT", pack_addr(depth, slot)),
            None => {
                let i = self.consts.add_str(name);
                self.emit1("STORE_GLOBAL", i as i64);
            }
        }
    }

    fn emit_typeof_name(&mut self, name: &str) -> R<()> {
        match self.resolve(name) {
            Some((depth, slot)) => self.emit1("TYPEOF_SLOT", pack_addr(depth, slot)),
            None => {
                let i = self.consts.add_str(name);
                self.emit1("TYPEOF_GLOBAL", i as i64);
            }
        }
        Ok(())
    }

    // ---- scopes ----

    fn open_scope(&mut self) {
        self.scopes.push(Scope::default());
        self.emit0("ENTER_SCOPE");
    }

    fn close_scope(&mut self) {
        self.emit0("EXIT_SCOPE");
        self.scopes.pop();
    }

    // ---- loops ----

    fn push_loop(
        &mut self,
        break_depth: usize,
        continue_depth: usize,
        is_switch: bool,
        label: Option<&str>,
    ) {
        let handler_base = self.fns.last().expect("frame").handlers.len();
        self.fns.last_mut().expect("frame").loops.push(LoopState {
            break_sites: Vec::new(),
            continue_sites: Vec::new(),
            break_depth,
            continue_depth,
            is_switch,
            iterator: None,
            label: label.map(str::to_string),
            handler_base,
        });
    }

    fn emit_close_iterator(&mut self, iterator: IteratorLoop) {
        let depth = self.scopes.len() - 1 - iterator.head_scope_index;
        self.emit1("LOAD_SLOT", pack_addr(depth, iterator.slot));
        self.emit0("DUP");
        let close = self.consts.add_str("close");
        self.emit1("GET_PROP", close as i64);
        self.emit1("CALL_METHOD", 0);
        self.emit0("POP");
    }

    fn close_iterators_for_return(&mut self) -> R<()> {
        let iterators: Vec<_> = self
            .fns
            .last()
            .expect("frame")
            .loops
            .iter()
            .filter_map(|loop_state| loop_state.iterator)
            .filter(|iterator| iterator.handler_active)
            .collect();
        // Close each open for-of iterator innermost first. Popping a handler just
        // before closing its iterator leaves every outer iterator handler live, so
        // if an inner `return()` throws, the next outer handler catches it, closes
        // its own iterator with the throw flag, and rethrows, which is what
        // IteratorClose does for a nested abrupt return. No try/finally sits
        // between the return and these iterators here: a return that crosses a
        // finally while an iterator is open declines earlier in compile_try.
        for iterator in iterators.into_iter().rev() {
            self.emit0("POP_HANDLER");
            self.emit_close_iterator(iterator);
        }
        Ok(())
    }

    fn pop_loop(&mut self) -> LoopState {
        self.fns
            .last_mut()
            .expect("frame")
            .loops
            .pop()
            .expect("open loop")
    }

    fn push_handler(&mut self, kind: HandlerKind) {
        self.fns.last_mut().expect("frame").handlers.push(kind);
    }

    fn pop_handler(&mut self) {
        self.fns.last_mut().expect("frame").handlers.pop();
    }

    /// A `break`/`continue` that leaves the finally body currently being
    /// compiled (its target loop is below the finally's floor) would abandon the
    /// pending completion record, which is not ported. Decline it.
    fn check_finally_escape(&self, idx: usize) -> R<()> {
        if let Some(&floor) = self.fns.last().expect("frame").finally_floors.last() {
            if idx < floor {
                return Err(Unsupported("break or continue out of a finally"));
            }
        }
        Ok(())
    }

    /// The handlers a jump out to `loops[idx]` crosses, innermost first.
    fn crossed_handlers(&self, idx: usize) -> Vec<HandlerKind> {
        let frame = self.fns.last().expect("frame");
        let base = frame.loops[idx].handler_base;
        frame.handlers[base..].iter().rev().copied().collect()
    }

    /// Emit the handler cleanup for a `break`/`continue` out to `loops[idx]`:
    /// pop a crossed catch, and for a crossed finally route the jump through the
    /// finally chain with ABRUPT_JUMP so it runs before the loop is left. Returns
    /// `Ok(true)` when it emitted an ABRUPT_JUMP trampoline that already jumped to
    /// `site_target` (so the caller must not also emit the plain JMP), `Ok(false)`
    /// for the simple path (caller emits the unwind + JMP). Declines when a for-of
    /// iterator is crossed, which needs iterator-close routing not ported here.
    fn emit_escape_cleanup(&mut self, idx: usize, target_depth: usize) -> R<Option<usize>> {
        let crossed = self.crossed_handlers(idx);
        if crossed.is_empty() {
            return Ok(None);
        }
        if crossed.iter().any(|h| matches!(h, HandlerKind::Iterator)) {
            return Err(Unsupported("break or continue across a for-of and a try"));
        }
        // The target loop's own for-of iterator would need closing on the way
        // out, which the simple pop and the finally trampoline do not do; decline
        // a cross-try jump out of a for-of.
        if self.fns.last().expect("frame").loops[idx]
            .iterator
            .is_some_and(|i| i.handler_active)
        {
            return Err(Unsupported(
                "break or continue across a try out of a for-of",
            ));
        }
        // Outermost crossed finally, if any: its try-scope is where the runtime
        // lands after the finally chain, so the trampoline exits from there.
        let outer_finally = crossed.iter().rev().find_map(|h| match h {
            HandlerKind::Finally { scope_depth } => Some(*scope_depth),
            HandlerKind::Catch => None,
            HandlerKind::Iterator => None,
        });
        match outer_finally {
            None => {
                // All crossed handlers are catch: pop each, then the caller
                // emits the scope unwind and the plain jump.
                for _ in &crossed {
                    self.emit0("POP_HANDLER");
                }
                Ok(None)
            }
            Some(finally_scope) => {
                // Route through the finally chain: ABRUPT_JUMP unwinds the
                // runtime handlers down to the loop's base, running each crossed
                // finally, then lands at a trampoline that exits the scopes left
                // over down to the loop target and jumps to it.
                let stop = self.fns.last().expect("frame").loops[idx].handler_base;
                let op_site = self.emit_jump("ABRUPT_JUMP");
                let tramp = self.code_len();
                for _ in target_depth..finally_scope {
                    self.emit0("EXIT_SCOPE");
                }
                let jump_site = self.emit_jump("JMP");
                let cidx = self.consts.add_jump_record(tramp as i64, stop as i64);
                self.patch_to(op_site, cidx);
                Ok(Some(jump_site))
            }
        }
    }

    fn emit_break(&mut self) -> R<()> {
        let idx = self
            .fns
            .last()
            .expect("frame")
            .loops
            .len()
            .checked_sub(1)
            .ok_or(Unsupported("break outside a loop or switch"))?;
        self.check_finally_escape(idx)?;
        let target = self.fns.last().expect("frame").loops[idx].break_depth;
        // A try crossed on the way out runs its finally and pops its catch. A
        // finally reroute already jumps to the loop exit, so record that site.
        if let Some(site) = self.emit_escape_cleanup(idx, target)? {
            self.fns.last_mut().expect("frame").loops[idx]
                .break_sites
                .push(site);
            return Ok(());
        }
        let iterator = self.fns.last().expect("frame").loops[idx].iterator;
        if let Some(iterator) = iterator.filter(|i| i.handler_active) {
            self.emit0("POP_HANDLER");
            self.emit_close_iterator(iterator);
        }
        self.unwind_scopes(target);
        let site = self.emit_jump("JMP");
        self.fns.last_mut().expect("frame").loops[idx]
            .break_sites
            .push(site);
        Ok(())
    }

    fn emit_continue(&mut self) -> R<()> {
        let idx = self
            .fns
            .last()
            .expect("frame")
            .loops
            .iter()
            .rposition(|l| !l.is_switch)
            .ok_or(Unsupported("continue outside a loop"))?;
        self.check_finally_escape(idx)?;
        let target = self.fns.last().expect("frame").loops[idx].continue_depth;
        if let Some(site) = self.emit_escape_cleanup(idx, target)? {
            self.fns.last_mut().expect("frame").loops[idx]
                .continue_sites
                .push(site);
            return Ok(());
        }
        let iterator = self.fns.last().expect("frame").loops[idx].iterator;
        if iterator.is_some_and(|i| i.handler_active) {
            self.emit0("POP_HANDLER");
        }
        self.unwind_scopes(target);
        let site = self.emit_jump("JMP");
        self.fns.last_mut().expect("frame").loops[idx]
            .continue_sites
            .push(site);
        Ok(())
    }

    fn emit_break_label(&mut self, label: &str) -> R<()> {
        let idx = self
            .fns
            .last()
            .expect("frame")
            .loops
            .iter()
            .rposition(|l| l.label.as_deref() == Some(label))
            .ok_or(Unsupported("labeled break target not found"))?;
        self.check_finally_escape(idx)?;
        let crossed = self.crossed_handlers(idx);
        let crosses_finally = crossed
            .iter()
            .any(|h| matches!(h, HandlerKind::Finally { .. }));
        let crosses_iterator = crossed.iter().any(|h| matches!(h, HandlerKind::Iterator));
        let target_is_for_of = self.fns.last().expect("frame").loops[idx]
            .iterator
            .is_some_and(|i| i.handler_active);
        // A labeled break that crosses a finally, with no for-of iterator crossed
        // and a non-for-of target, routes through the same finally trampoline the
        // unlabeled break uses: ABRUPT_JUMP runs each crossed finally, then a
        // trampoline exits the leftover scopes and jumps to the loop exit.
        if crosses_finally && !crosses_iterator && !target_is_for_of {
            let target = self.fns.last().expect("frame").loops[idx].break_depth;
            let site = self
                .emit_escape_cleanup(idx, target)?
                .expect("a crossed finally yields a trampoline jump");
            self.fns.last_mut().expect("frame").loops[idx]
                .break_sites
                .push(site);
            return Ok(());
        }
        // A crossed catch, or a finally combined with a for-of iterator, is not
        // routed through the trampoline; decline.
        if crossed.iter().any(|h| !matches!(h, HandlerKind::Iterator)) {
            return Err(Unsupported("labeled break across a try"));
        }
        // Close every active iterator of a loop this break fully exits, innermost
        // first. More than one would need atomic multi-iterator unwinding, the
        // same hazard `return across nested iterators` declines, so decline that.
        let to_close: Vec<IteratorLoop> = self.fns.last().expect("frame").loops[idx..]
            .iter()
            .rev()
            .filter_map(|l| l.iterator.filter(|i| i.handler_active))
            .collect();
        if to_close.len() > 1 {
            return Err(Unsupported("labeled break across nested iterators"));
        }
        for iterator in to_close {
            self.emit0("POP_HANDLER");
            self.emit_close_iterator(iterator);
        }
        let target = self.fns.last().expect("frame").loops[idx].break_depth;
        self.unwind_scopes(target);
        let site = self.emit_jump("JMP");
        self.fns.last_mut().expect("frame").loops[idx]
            .break_sites
            .push(site);
        Ok(())
    }

    fn emit_continue_label(&mut self, label: &str) -> R<()> {
        let idx = self
            .fns
            .last()
            .expect("frame")
            .loops
            .iter()
            .rposition(|l| l.label.as_deref() == Some(label))
            .ok_or(Unsupported("labeled continue target not found"))?;
        self.check_finally_escape(idx)?;
        if self.fns.last().expect("frame").loops[idx].is_switch {
            // `continue` to a `switch` or a labeled block is a syntax error the
            // parser rejects; decline rather than emit a bad jump if it reaches
            // here.
            return Err(Unsupported("continue to a non-loop label"));
        }
        let crossed = self.crossed_handlers(idx);
        let crosses_finally = crossed
            .iter()
            .any(|h| matches!(h, HandlerKind::Finally { .. }));
        let crosses_iterator = crossed.iter().any(|h| matches!(h, HandlerKind::Iterator));
        let target_is_for_of = self.fns.last().expect("frame").loops[idx]
            .iterator
            .is_some_and(|i| i.handler_active);
        // A labeled continue that crosses a finally, with no for-of iterator
        // crossed and a non-for-of target, routes through the finally trampoline
        // like the unlabeled continue, landing at the loop's continue target.
        if crosses_finally && !crosses_iterator && !target_is_for_of {
            let target = self.fns.last().expect("frame").loops[idx].continue_depth;
            let site = self
                .emit_escape_cleanup(idx, target)?
                .expect("a crossed finally yields a trampoline jump");
            self.fns.last_mut().expect("frame").loops[idx]
                .continue_sites
                .push(site);
            return Ok(());
        }
        if crossed.iter().any(|h| !matches!(h, HandlerKind::Iterator)) {
            return Err(Unsupported("labeled continue across a try"));
        }
        // Inner loops this continue fully exits have their iterators closed; the
        // target loop keeps iterating, so its handler is only popped (the loop
        // top re-pushes it). Crossing more than one handler would need atomic
        // multi-iterator unwinding, so decline that, matching the break case.
        let loops = &self.fns.last().expect("frame").loops;
        let inner_close: Vec<IteratorLoop> = loops[idx + 1..]
            .iter()
            .rev()
            .filter_map(|l| l.iterator.filter(|i| i.handler_active))
            .collect();
        let target_pop = loops[idx].iterator.is_some_and(|i| i.handler_active);
        if inner_close.len() + usize::from(target_pop) > 1 {
            return Err(Unsupported("labeled continue across nested iterators"));
        }
        for iterator in inner_close {
            self.emit0("POP_HANDLER");
            self.emit_close_iterator(iterator);
        }
        if target_pop {
            self.emit0("POP_HANDLER");
        }
        let target = self.fns.last().expect("frame").loops[idx].continue_depth;
        self.unwind_scopes(target);
        let site = self.emit_jump("JMP");
        self.fns.last_mut().expect("frame").loops[idx]
            .continue_sites
            .push(site);
        Ok(())
    }

    /// Emit `EXIT_SCOPE` for every block scope open between here and `target`
    /// depth, so a `break`/`continue` that jumps out of blocks leaves the runtime
    /// scope chain at the depth its landing site expects.
    fn unwind_scopes(&mut self, target: usize) {
        for _ in target..self.scopes.len() {
            self.emit0("EXIT_SCOPE");
        }
    }

    // ---- emit ----

    fn code_len(&self) -> usize {
        self.fns.last().expect("frame").code.len()
    }

    fn emit0(&mut self, name: &str) {
        debug_assert_eq!(super::opcodes::arity(name), 0, "{name} is not nullary");
        let byte = self.layout.byte_of(name) as i64;
        self.fns.last_mut().expect("frame").code.push(byte);
    }

    fn emit1(&mut self, name: &str, operand: i64) {
        debug_assert_eq!(super::opcodes::arity(name), 1, "{name} takes no operand");
        let byte = self.layout.byte_of(name) as i64;
        let code = &mut self.fns.last_mut().expect("frame").code;
        code.push(byte);
        code.push(operand);
    }

    /// Emit a jump opcode with a placeholder target, and return the code index of
    /// the operand to patch once the target is known.
    fn emit_jump(&mut self, name: &str) -> usize {
        let byte = self.layout.byte_of(name) as i64;
        let code = &mut self.fns.last_mut().expect("frame").code;
        code.push(byte);
        let slot = code.len();
        code.push(0);
        slot
    }

    fn patch_here(&mut self, slot: usize) {
        let target = self.code_len();
        self.patch_to(slot, target);
    }

    fn patch_to(&mut self, slot: usize, target: usize) {
        self.fns.last_mut().expect("frame").code[slot] = target as i64;
    }

    fn patch_all(&mut self, slots: &[usize], target: usize) {
        for &slot in slots {
            self.patch_to(slot, target);
        }
    }
}

/// Pack a slot address the way the runtime unpacks it: `depth << 16 | index`.
fn collect_var_decl(
    decl: &oxc_ast::ast::VariableDeclaration<'_>,
    names: &mut Vec<String>,
) -> R<()> {
    if decl.kind != VariableDeclarationKind::Var {
        return Ok(());
    }
    for d in &decl.declarations {
        collect_pattern_names(&d.id, names)?;
    }
    Ok(())
}

/// Collect every binding name a pattern introduces, deduped, in source order.
/// Used to declare lexical slots and to hoist `var` names for a destructuring
/// declaration. Computed keys and default expressions bind no name here; only
/// the leaf identifiers do.
fn collect_pattern_names(pat: &BindingPattern<'_>, out: &mut Vec<String>) -> R<()> {
    match pat {
        BindingPattern::BindingIdentifier(id) => {
            let name = id.name.as_str();
            if !out.iter().any(|existing| existing == name) {
                out.push(name.to_string());
            }
        }
        BindingPattern::AssignmentPattern(ap) => collect_pattern_names(&ap.left, out)?,
        BindingPattern::ArrayPattern(arr) => {
            for element in arr.elements.iter().flatten() {
                collect_pattern_names(element, out)?;
            }
            if let Some(rest) = &arr.rest {
                collect_pattern_names(&rest.argument, out)?;
            }
        }
        BindingPattern::ObjectPattern(obj) => {
            for prop in &obj.properties {
                collect_pattern_names(&prop.value, out)?;
            }
            if let Some(rest) = &obj.rest {
                collect_pattern_names(&rest.argument, out)?;
            }
        }
    }
    Ok(())
}

fn collect_function_vars(stmts: &[Statement<'_>], names: &mut Vec<String>) -> R<()> {
    for statement in stmts {
        collect_function_var_stmt(statement, names)?;
    }
    Ok(())
}

fn collect_function_var_stmt(stmt: &Statement<'_>, names: &mut Vec<String>) -> R<()> {
    match stmt {
        Statement::VariableDeclaration(decl) => collect_var_decl(decl, names)?,
        Statement::BlockStatement(block) => collect_function_vars(&block.body, names)?,
        Statement::IfStatement(branch) => {
            collect_function_var_stmt(&branch.consequent, names)?;
            if let Some(else_branch) = &branch.alternate {
                collect_function_var_stmt(else_branch, names)?;
            }
        }
        Statement::WhileStatement(loop_stmt) => {
            collect_function_var_stmt(&loop_stmt.body, names)?;
        }
        Statement::DoWhileStatement(loop_stmt) => {
            collect_function_var_stmt(&loop_stmt.body, names)?;
        }
        Statement::ForStatement(loop_stmt) => {
            if let Some(ForStatementInit::VariableDeclaration(decl)) = &loop_stmt.init {
                collect_var_decl(decl, names)?;
            }
            collect_function_var_stmt(&loop_stmt.body, names)?;
        }
        Statement::ForOfStatement(loop_stmt) => {
            if let ForStatementLeft::VariableDeclaration(decl) = &loop_stmt.left {
                collect_var_decl(decl, names)?;
            }
            collect_function_var_stmt(&loop_stmt.body, names)?;
        }
        Statement::ForInStatement(loop_stmt) => {
            if let ForStatementLeft::VariableDeclaration(decl) = &loop_stmt.left {
                collect_var_decl(decl, names)?;
            }
            collect_function_var_stmt(&loop_stmt.body, names)?;
        }
        Statement::SwitchStatement(switch_stmt) => {
            for case in &switch_stmt.cases {
                collect_function_vars(&case.consequent, names)?;
            }
        }
        Statement::TryStatement(try_stmt) => {
            collect_function_vars(&try_stmt.block.body, names)?;
            if let Some(catch) = &try_stmt.handler {
                collect_function_vars(&catch.body.body, names)?;
            }
            if let Some(finalizer) = &try_stmt.finalizer {
                collect_function_vars(&finalizer.body, names)?;
            }
        }
        Statement::LabeledStatement(labeled) => {
            collect_function_var_stmt(&labeled.body, names)?;
        }
        // Nested functions and classes have separate var environments. Other
        // unrecognized statements are rejected by the actual compiler.
        _ => {}
    }
    Ok(())
}

fn contains_return_stmt(stmts: &[Statement<'_>]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Statement::ReturnStatement(_) => true,
        Statement::BlockStatement(block) => contains_return_stmt(&block.body),
        Statement::IfStatement(s) => {
            contains_return_stmt(std::slice::from_ref(&s.consequent))
                || s.alternate
                    .as_ref()
                    .is_some_and(|alt| contains_return_stmt(std::slice::from_ref(alt)))
        }
        Statement::WhileStatement(s) => contains_return_stmt(std::slice::from_ref(&s.body)),
        Statement::DoWhileStatement(s) => contains_return_stmt(std::slice::from_ref(&s.body)),
        Statement::ForStatement(s) => contains_return_stmt(std::slice::from_ref(&s.body)),
        Statement::ForOfStatement(s) => contains_return_stmt(std::slice::from_ref(&s.body)),
        Statement::ForInStatement(s) => contains_return_stmt(std::slice::from_ref(&s.body)),
        Statement::SwitchStatement(s) => s
            .cases
            .iter()
            .any(|case| contains_return_stmt(&case.consequent)),
        Statement::TryStatement(s) => {
            contains_return_stmt(&s.block.body)
                || s.handler
                    .as_ref()
                    .is_some_and(|h| contains_return_stmt(&h.body.body))
                || s.finalizer
                    .as_ref()
                    .is_some_and(|b| contains_return_stmt(&b.body))
        }
        _ => false,
    })
}

/// Whether a `yield`, `yield*`, or `await` appears anywhere inside a node. A
/// `try` or a `for`/`of`/`for`/`in` inside a generator keeps a handler (a finally
/// or an iterator) live only while its body runs. If the generator suspends there
/// on a `yield` (or an `await` in an async generator), a consumer's `return` or
/// `throw` at that suspension would have to run the finally or close the iterator,
/// which the runtime does not port, so the construct declines. Without a
/// suspension inside, the handler pushes and pops within one resume and behaves
/// exactly as in an ordinary function, so it compiles. The scan reaches into a
/// nested function too, which only ever declines more, never less.
struct SuspendScan {
    found: bool,
}

impl<'a> Visit<'a> for SuspendScan {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if matches!(
            kind,
            AstKind::YieldExpression(_) | AstKind::AwaitExpression(_)
        ) {
            self.found = true;
        }
    }
}

fn try_statement_suspends(stmt: &TryStatement<'_>) -> bool {
    let mut scan = SuspendScan { found: false };
    scan.visit_try_statement(stmt);
    scan.found
}

fn statement_suspends(stmt: &Statement<'_>) -> bool {
    let mut scan = SuspendScan { found: false };
    scan.visit_statement(stmt);
    scan.found
}

fn pack_addr(depth: usize, index: usize) -> i64 {
    ((depth as i64) << 16) | (index as i64)
}

/// The TDZ kind tag for a lexical declaration (1 = let, 2 = const), or
/// [`Unsupported`] for `var`/`using`.
fn lexical_kind(kind: VariableDeclarationKind) -> R<i64> {
    match kind {
        VariableDeclarationKind::Let => Ok(1),
        VariableDeclarationKind::Const => Ok(2),
        _ => Err(Unsupported("var/using declaration")),
    }
}

/// The identifier of a plain binding, or [`Unsupported`] for a destructuring
/// pattern.
fn binding_identifier<'a, 'b>(
    pat: &'b BindingPattern<'a>,
) -> R<&'b oxc_ast::ast::BindingIdentifier<'a>> {
    match pat {
        BindingPattern::BindingIdentifier(id) => Ok(id),
        _ => Err(Unsupported("destructuring binding")),
    }
}

/// Whether a top-level statement binds a name that a classic script would
/// expose on the page: a function or class declaration, or a `var`/`let`/`const`
/// declaration. These become page globals, so a chunk with no module syntax
/// declines (see [`Compiler::compile_main`]). A statement that binds nothing at
/// the top level (an expression, an `if`, a loop, a bare block) is not one.
fn is_top_level_declaration(statement: &Statement<'_>) -> bool {
    matches!(
        statement,
        Statement::FunctionDeclaration(_)
            | Statement::ClassDeclaration(_)
            | Statement::VariableDeclaration(_)
    )
}

/// The spelling of one side of an export specifier. A local is an identifier
/// reference; a public name is an identifier or a string literal (`export { a as
/// "x" }`). A string with a lone surrogate (only a built AST can hold one)
/// declines.
fn module_export_name<'a>(name: &'a ModuleExportName<'a>) -> R<&'a str> {
    match name {
        ModuleExportName::IdentifierName(id) => Ok(id.name.as_str()),
        ModuleExportName::IdentifierReference(id) => Ok(id.name.as_str()),
        ModuleExportName::StringLiteral(s) => s
            .value
            .as_str()
            .ok_or(Unsupported("export name with a lone surrogate")),
    }
}

/// A local `export <decl>` is supported only for `const` with plain identifier
/// bindings. A `const` is never reassigned, so the wrapper can re-export its
/// final value after the module runs without missing a later mutation. Every
/// other export declaration, and `export { ... }` specifiers, decline.
/// Whether an expression is an anonymous class expression, whose `.name` a
/// named-evaluation position would set to its binding or key. In a position that
/// cannot supply that name here, an observed-name chunk declines instead of
/// shipping the empty string.
fn is_anonymous_class(expr: &Expression<'_>) -> bool {
    matches!(expr, Expression::ClassExpression(class) if class.id.is_none())
}

fn export_const_declaration<'a, 'b>(
    ex: &'b oxc_ast::ast::ExportDeclaration<'a>,
) -> R<&'b oxc_ast::ast::VariableDeclaration<'a>> {
    match &ex.declaration {
        oxc_ast::ast::Declaration::VariableDeclaration(decl)
            if decl.kind == VariableDeclarationKind::Const =>
        {
            Ok(decl)
        }
        _ => Err(Unsupported("export declaration outside the subset")),
    }
}

/// The plain identifier names of a parameter list, or [`Unsupported`] for rest,
/// default, or destructured parameters.
/// Whether the chunk observes a callable's name, so dropping function and class
/// names would change what it sees. It is true when any `.name` or `.constructor`
/// member access, any `"name"` or `"constructor"` string (a computed access like
/// `obj["name"]`), or `new.target` appears. When it is false, every function and
/// class name is dropped from the emitted program, so a decoded program carries
/// no original identifier and `fn.name`/`constructor.name` read as the empty
/// string, which nothing in the chunk looks at. The scan is deliberately broad:
/// a false positive keeps a name, which is only less concealment, never wrong.
/// This mirrors the mangle pass's `reads_callable_names`, with `.constructor`
/// added because the VM also drops the class descriptor name.
fn names_observable(program: &Program<'_>) -> bool {
    struct Scan {
        found: bool,
    }
    impl<'a> Visit<'a> for Scan {
        fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
            if it.property.name == "name" || it.property.name == "constructor" {
                self.found = true;
            }
            oxc_ast_visit::walk::walk_static_member_expression(self, it);
        }
        fn visit_string_literal(&mut self, it: &oxc_ast::ast::StringLiteral<'a>) {
            if matches!(it.value.as_str(), Some("name") | Some("constructor")) {
                self.found = true;
            }
        }
        fn visit_new_target(&mut self, _it: &oxc_ast::ast::NewTarget) {
            // `new.target` is the running constructor, so the chunk can reach its
            // name. `import.meta` is a separate node and does not count.
            self.found = true;
        }
    }
    let mut scan = Scan { found: false };
    scan.visit_program(program);
    scan.found
}

/// Whether a function body references `arguments` as a free name. It matches an
/// identifier reference only, so `obj.arguments` (a property) and `{ arguments:
/// 1 }` (a key) do not count. Nested function bodies are not pruned, so a nested
/// function's own `arguments` can raise this; that only makes the enclosing
/// function build an unused arguments object, which is harmless.
fn references_arguments(stmts: &[Statement<'_>]) -> bool {
    struct Scan {
        found: bool,
    }
    impl<'a> Visit<'a> for Scan {
        fn visit_identifier_reference(&mut self, it: &oxc_ast::ast::IdentifierReference<'a>) {
            if it.name == "arguments" {
                self.found = true;
            }
        }
    }
    let mut scan = Scan { found: false };
    for stmt in stmts {
        scan.visit_statement(stmt);
    }
    scan.found
}

/// Whether a parameter is named `arguments`, so the parameter itself binds the
/// name and no arguments object is synthesized.
fn params_bind_arguments(params: &FormalParameters<'_>) -> bool {
    let item = params
        .items
        .iter()
        .any(|p| binding_identifier(&p.pattern).is_ok_and(|id| id.name == "arguments"));
    let rest = params.rest.as_ref().is_some_and(|r| {
        binding_identifier(&r.rest.argument).is_ok_and(|id| id.name == "arguments")
    });
    item || rest
}

/// Whether a function body declares `arguments` with a `var`, lexical, function,
/// or class binding. Such a binding would shadow a synthesized arguments object,
/// which the slot model does not express, so the function declines instead.
fn body_binds_arguments(stmts: &[Statement<'_>]) -> bool {
    let mut vars = Vec::new();
    if collect_function_vars(stmts, &mut vars).is_ok() && vars.iter().any(|n| n == "arguments") {
        return true;
    }
    stmts.iter().any(|s| match s {
        Statement::FunctionDeclaration(f) => f.id.as_ref().is_some_and(|id| id.name == "arguments"),
        Statement::ClassDeclaration(c) => c.id.as_ref().is_some_and(|id| id.name == "arguments"),
        Statement::VariableDeclaration(d) if !matches!(d.kind, VariableDeclarationKind::Var) => d
            .declarations
            .iter()
            .any(|dc| binding_identifier(&dc.id).is_ok_and(|id| id.name == "arguments")),
        _ => false,
    })
}

/// Whether a block's statement list introduces a lexical scope (a `let`/`const`,
/// a function declaration, or a class declaration directly inside it).
fn block_needs_scope(stmts: &[Statement<'_>]) -> bool {
    stmts.iter().any(|s| match s {
        Statement::VariableDeclaration(d) => {
            matches!(
                d.kind,
                VariableDeclarationKind::Let | VariableDeclarationKind::Const
            )
        }
        Statement::FunctionDeclaration(_) | Statement::ClassDeclaration(_) => true,
        _ => false,
    })
}

/// The opcode for a binary operator, or [`Unsupported`] for one with no plain
/// binary opcode.
fn binary_op(op: BinaryOperator) -> R<&'static str> {
    Ok(match op {
        BinaryOperator::Addition => "ADD",
        BinaryOperator::Subtraction => "SUB",
        BinaryOperator::Multiplication => "MUL",
        BinaryOperator::Division => "DIV",
        BinaryOperator::Remainder => "MOD",
        BinaryOperator::Exponential => "EXP",
        BinaryOperator::Equality => "EQ",
        BinaryOperator::Inequality => "NEQ",
        BinaryOperator::StrictEquality => "SEQ",
        BinaryOperator::StrictInequality => "SNEQ",
        BinaryOperator::LessThan => "LT",
        BinaryOperator::LessEqualThan => "LTE",
        BinaryOperator::GreaterThan => "GT",
        BinaryOperator::GreaterEqualThan => "GTE",
        BinaryOperator::BitwiseAnd => "BAND",
        BinaryOperator::BitwiseOR => "BOR",
        BinaryOperator::BitwiseXOR => "BXOR",
        BinaryOperator::ShiftLeft => "SHL",
        BinaryOperator::ShiftRight => "SHR",
        BinaryOperator::ShiftRightZeroFill => "USHR",
        BinaryOperator::Instanceof => "INSTANCEOF",
        BinaryOperator::In => "IN",
    })
}

/// The binary opcode for a compound assignment operator (`+=` and friends), or
/// [`Unsupported`] for one estrogen does not lower.
fn compound_binary_op(op: AssignmentOperator) -> R<&'static str> {
    Ok(match op {
        AssignmentOperator::Addition => "ADD",
        AssignmentOperator::Subtraction => "SUB",
        AssignmentOperator::Multiplication => "MUL",
        AssignmentOperator::Division => "DIV",
        AssignmentOperator::Remainder => "MOD",
        AssignmentOperator::Exponential => "EXP",
        AssignmentOperator::ShiftLeft => "SHL",
        AssignmentOperator::ShiftRight => "SHR",
        AssignmentOperator::ShiftRightZeroFill => "USHR",
        AssignmentOperator::BitwiseOR => "BOR",
        AssignmentOperator::BitwiseXOR => "BXOR",
        AssignmentOperator::BitwiseAnd => "BAND",
        _ => return Err(Unsupported("compound assignment operator")),
    })
}

/// The string key of a non-computed object property, or `None` for a key estrogen
/// does not lower (computed, private, a complex expression).
fn static_key(key: &PropertyKey<'_>) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str().to_string()),
        PropertyKey::StringLiteral(s) => s.value.as_str().map(|v| v.to_string()),
        _ => None,
    }
}

// ---- closure detection (for the for-loop lexical-head soundness guard) ----
//
// These answer one question conservatively: could this subtree create a closure?
// A yes forces a `for (let ...)` to decline, because estrogen uses one binding
// per loop variable and a closure would observe the difference from native's
// per-iteration binding. Anything not recognized returns true (decline), so the
// guard never misses a closure.

fn has_closure_expr(e: &Expression<'_>) -> bool {
    match e {
        Expression::FunctionExpression(_)
        | Expression::ArrowFunctionExpression(_)
        | Expression::ClassExpression(_) => true,
        Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::RegExpLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::Identifier(_)
        | Expression::ThisExpression(_) => false,
        Expression::TemplateLiteral(t) => t.expressions.iter().any(has_closure_expr),
        Expression::ParenthesizedExpression(p) => has_closure_expr(&p.expression),
        Expression::UnaryExpression(u) => has_closure_expr(&u.argument),
        Expression::UpdateExpression(u) => simple_target_has_closure(&u.argument),
        Expression::BinaryExpression(b) => has_closure_expr(&b.left) || has_closure_expr(&b.right),
        Expression::LogicalExpression(l) => has_closure_expr(&l.left) || has_closure_expr(&l.right),
        Expression::ConditionalExpression(c) => {
            has_closure_expr(&c.test)
                || has_closure_expr(&c.consequent)
                || has_closure_expr(&c.alternate)
        }
        Expression::SequenceExpression(s) => s.expressions.iter().any(has_closure_expr),
        Expression::AssignmentExpression(a) => {
            has_closure_expr(&a.right) || target_has_closure(&a.left)
        }
        Expression::StaticMemberExpression(m) => has_closure_expr(&m.object),
        Expression::ComputedMemberExpression(m) => {
            has_closure_expr(&m.object) || has_closure_expr(&m.expression)
        }
        Expression::CallExpression(c) => {
            has_closure_expr(&c.callee) || c.arguments.iter().any(arg_has_closure)
        }
        Expression::NewExpression(n) => {
            has_closure_expr(&n.callee) || n.arguments.iter().any(arg_has_closure)
        }
        Expression::ArrayExpression(a) => a.elements.iter().any(|el| match el {
            ArrayExpressionElement::Elision(_) => false,
            other => other.as_expression().is_none_or(has_closure_expr),
        }),
        Expression::ObjectExpression(o) => o.properties.iter().any(|p| match p {
            ObjectPropertyKind::ObjectProperty(op) => {
                op.method
                    || op.computed
                    || op.kind != PropertyKind::Init
                    || has_closure_expr(&op.value)
            }
            ObjectPropertyKind::SpreadProperty(s) => has_closure_expr(&s.argument),
        }),
        // await and yield are not closures; a closure is only in their argument.
        Expression::AwaitExpression(a) => has_closure_expr(&a.argument),
        Expression::YieldExpression(y) => y.argument.as_ref().is_some_and(has_closure_expr),
        _ => true,
    }
}

fn arg_has_closure(a: &Argument<'_>) -> bool {
    a.as_expression().is_none_or(has_closure_expr)
}

fn simple_target_has_closure(t: &SimpleAssignmentTarget<'_>) -> bool {
    t.as_member_expression().is_some_and(member_has_closure)
}

fn target_has_closure(t: &AssignmentTarget<'_>) -> bool {
    if let AssignmentTarget::AssignmentTargetIdentifier(_) = t {
        return false;
    }
    t.as_member_expression().is_none_or(member_has_closure)
}

fn member_has_closure(me: &MemberExpression<'_>) -> bool {
    match me {
        MemberExpression::StaticMemberExpression(m) => has_closure_expr(&m.object),
        MemberExpression::ComputedMemberExpression(m) => {
            has_closure_expr(&m.object) || has_closure_expr(&m.expression)
        }
        MemberExpression::PrivateFieldExpression(m) => has_closure_expr(&m.object),
    }
}

fn has_closure_stmt(s: &Statement<'_>) -> bool {
    match s {
        Statement::EmptyStatement(_)
        | Statement::BreakStatement(_)
        | Statement::ContinueStatement(_) => false,
        Statement::ExpressionStatement(e) => has_closure_expr(&e.expression),
        Statement::VariableDeclaration(d) => d
            .declarations
            .iter()
            .any(|dc| dc.init.as_ref().is_some_and(has_closure_expr)),
        Statement::BlockStatement(b) => b.body.iter().any(has_closure_stmt),
        Statement::IfStatement(i) => {
            has_closure_expr(&i.test)
                || has_closure_stmt(&i.consequent)
                || i.alternate.as_ref().is_some_and(|a| has_closure_stmt(a))
        }
        Statement::WhileStatement(w) => has_closure_expr(&w.test) || has_closure_stmt(&w.body),
        Statement::DoWhileStatement(w) => has_closure_expr(&w.test) || has_closure_stmt(&w.body),
        Statement::ForStatement(f) => {
            f.init.as_ref().is_some_and(for_init_has_closure)
                || f.test.as_ref().is_some_and(|e| has_closure_expr(e))
                || f.update.as_ref().is_some_and(|e| has_closure_expr(e))
                || has_closure_stmt(&f.body)
        }
        Statement::SwitchStatement(sw) => {
            has_closure_expr(&sw.discriminant)
                || sw.cases.iter().any(|c| {
                    c.test.as_ref().is_some_and(|e| has_closure_expr(e))
                        || c.consequent.iter().any(has_closure_stmt)
                })
        }
        Statement::ReturnStatement(r) => r.argument.as_ref().is_some_and(has_closure_expr),
        // A for-of/in body or iterable can hold a closure that captures the outer
        // for-let binding. The head's binding target cannot hide one that matters
        // here: the only target the VM compiles is a single plain identifier (a
        // destructuring or assignment-target head declines in compile_iterated),
        // so recursing into the iterable and body is sound.
        Statement::ForOfStatement(f) => has_closure_expr(&f.right) || has_closure_stmt(&f.body),
        Statement::ForInStatement(f) => has_closure_expr(&f.right) || has_closure_stmt(&f.body),
        Statement::LabeledStatement(l) => has_closure_stmt(&l.body),
        Statement::ThrowStatement(t) => has_closure_expr(&t.argument),
        // A destructuring catch binding declines in compile_try, so only the
        // block, catch, and finally bodies can hold a closure that reaches here.
        Statement::TryStatement(t) => {
            t.block.body.iter().any(has_closure_stmt)
                || t.handler
                    .as_ref()
                    .is_some_and(|h| h.body.body.iter().any(has_closure_stmt))
                || t.finalizer
                    .as_ref()
                    .is_some_and(|f| f.body.iter().any(has_closure_stmt))
        }
        _ => true,
    }
}

fn for_init_has_closure(i: &ForStatementInit<'_>) -> bool {
    match i {
        ForStatementInit::VariableDeclaration(d) => d
            .declarations
            .iter()
            .any(|dc| dc.init.as_ref().is_some_and(has_closure_expr)),
        other => other.as_expression().is_none_or(has_closure_expr),
    }
}
