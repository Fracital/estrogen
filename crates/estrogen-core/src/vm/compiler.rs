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
//!   plain identifier bindings, and function declarations.
//! - Functions: declarations, anonymous function expressions, and arrow
//!   functions (expression or block body), with plain identifier parameters,
//!   `return`, recursion, nested functions, and lexical closure capture. `this`
//!   is the call-site receiver for a plain function and the defining `this` for
//!   an arrow.
//! - Control flow: `if`/`else`, `while`, `do`/`while`, C-style `for`, `switch`,
//!   and unlabeled `break`/`continue`.
//! - Expressions over literals, identifiers (locals resolve to slots, free names
//!   to globals), member access, calls and method calls, `new`, unary and binary
//!   operators, logical and conditional operators, assignment (plain, compound,
//!   and logical to an identifier; plain to a member), update (`++`/`--` on an
//!   identifier), array and object literals, and sequence expressions.
//!
//! Anything else (classes, `import`/`export`, `var`, generators, `async`,
//! destructuring, default/rest parameters, named function expressions,
//! `arguments`, labeled break/continue, `try`/`catch`/`finally`, for-of/for-in,
//! optional chaining, spread, templates with substitutions, and update on a
//! member) falls back.
//!
//! Locals are slot-based exactly as the runtime expects. A frame has one base
//! scope (its parameters and body-level lexicals); a lexical block pushes a
//! child scope (`ENTER_SCOPE`/`EXIT_SCOPE`). A name resolves to a packed
//! `(depth, index)` slot address, where `depth` counts scope hops up the runtime
//! chain, which spans the closure boundary because a closure's base scope points
//! at the defining frame's scope. A free name compiles to a `*_GLOBAL` op.

use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, AssignmentExpression, AssignmentOperator,
    AssignmentTarget, BinaryOperator, BindingPattern, Expression, ForStatement, ForStatementInit,
    FormalParameters, Function, IfStatement, LogicalOperator, MemberExpression, ObjectPropertyKind,
    Program, PropertyKey, PropertyKind, SimpleAssignmentTarget, Statement, SwitchStatement,
    UnaryOperator, UpdateOperator, VariableDeclarationKind,
};

use super::layout::Layout;
use super::program::{ConstPool, FunctionDef, Program as VmProgram};

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
pub fn compile_program(program: &Program<'_>, seed: &str) -> R<VmProgram> {
    let mut c = Compiler {
        layout: Layout::build(seed),
        consts: ConstPool::default(),
        functions: Vec::new(),
        scopes: Vec::new(),
        fns: Vec::new(),
    };
    let main_index = c.compile_main(program)?;
    Ok(VmProgram {
        layout: c.layout,
        consts: c.consts,
        functions: c.functions,
        main_index,
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
struct FnState {
    code: Vec<i64>,
    loops: Vec<LoopState>,
    is_module_main: bool,
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
}

impl Compiler {
    // ---- function compilation ----

    fn compile_main(&mut self, program: &Program<'_>) -> R<usize> {
        self.fns.push(FnState {
            code: Vec::new(),
            loops: Vec::new(),
            is_module_main: true,
        });
        self.scopes.push(Scope::default());
        self.compile_scope_body(&program.body, true)?;
        self.emit0("HALT");
        self.scopes.pop();
        let st = self.fns.pop().expect("main frame");
        Ok(self.add_function(FunctionDef {
            code: st.code,
            name: None,
            is_async: false,
            is_arrow: false,
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
    ) -> R<usize> {
        let param_names = parse_params(params)?;
        let num_params = param_names.len();
        self.fns.push(FnState {
            code: Vec::new(),
            loops: Vec::new(),
            is_module_main: false,
        });
        self.scopes.push(Scope::default());
        for (arg_index, pn) in param_names.iter().enumerate() {
            let slot = self.add_local(pn);
            self.emit1("BIND_PARAM", ((slot as i64) << 8) | (arg_index as i64));
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
            is_async: false,
            is_arrow,
            num_params,
        }))
    }

    fn add_function(&mut self, def: FunctionDef) -> usize {
        let idx = self.functions.len();
        self.functions.push(def);
        idx
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
                    let kind = lexical_kind(decl.kind)?;
                    for d in &decl.declarations {
                        let id = binding_identifier(&d.id)?;
                        let slot = self.add_local(id.name.as_str());
                        self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                    }
                }
                Statement::FunctionDeclaration(func) => {
                    if func.r#async || func.generator {
                        return Err(Unsupported("async/generator function"));
                    }
                    let name = func
                        .id
                        .as_ref()
                        .ok_or(Unsupported("function declaration without a name"))?
                        .name
                        .as_str()
                        .to_string();
                    let slot = self.add_local(&name);
                    hoisted.push((slot, func));
                }
                Statement::ClassDeclaration(_) => return Err(Unsupported("class declaration")),
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
            let idx =
                self.compile_function(&func.params, FnBody::Block(&fb.statements), name, false)?;
            self.emit1("MAKE_FUNC", idx as i64);
            self.emit1("DEF_FUNC_SLOT", slot as i64);
        }
        // Statements in order. Function declarations are already defined.
        for stmt in stmts {
            if matches!(stmt, Statement::FunctionDeclaration(_)) {
                continue;
            }
            self.compile_statement(stmt, top_level)?;
        }
        Ok(())
    }

    // ---- statements ----

    fn compile_statement(&mut self, stmt: &Statement<'_>, top_level: bool) -> R<()> {
        match stmt {
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
                lexical_kind(decl.kind)?;
                for d in &decl.declarations {
                    let id = binding_identifier(&d.id)?;
                    let (depth, slot) = self
                        .resolve(id.name.as_str())
                        .ok_or(Unsupported("binding not declared in its scope"))?;
                    debug_assert_eq!(depth, 0, "a declaration initializes a current-scope slot");
                    match &d.init {
                        Some(init) => self.compile_expr(init)?,
                        None => self.emit0("PUSH_UNDEF"),
                    }
                    self.emit1("INIT_SLOT", pack_addr(0, slot));
                }
                Ok(())
            }
            Statement::BlockStatement(b) => self.compile_block(&b.body),
            Statement::IfStatement(s) => self.compile_if(s),
            Statement::WhileStatement(s) => self.compile_while(&s.test, &s.body),
            Statement::DoWhileStatement(s) => self.compile_do_while(&s.body, &s.test),
            Statement::ForStatement(s) => self.compile_for(s),
            Statement::SwitchStatement(s) => self.compile_switch(s),
            Statement::ReturnStatement(s) => {
                if self.fns.last().expect("frame").is_module_main {
                    return Err(Unsupported("return at the top level"));
                }
                match &s.argument {
                    Some(arg) => {
                        self.compile_expr(arg)?;
                        self.emit0("RETURN");
                    }
                    None => self.emit0("RETURN_UNDEF"),
                }
                Ok(())
            }
            Statement::BreakStatement(s) => {
                if s.label.is_some() {
                    return Err(Unsupported("labeled break"));
                }
                self.emit_break()
            }
            Statement::ContinueStatement(s) => {
                if s.label.is_some() {
                    return Err(Unsupported("labeled continue"));
                }
                self.emit_continue()
            }
            Statement::FunctionDeclaration(_) => Err(Unsupported(
                "function declaration outside a statement-list top",
            )),
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

    fn compile_while(&mut self, test: &Expression<'_>, body: &Statement<'_>) -> R<()> {
        let l_top = self.code_len();
        let depth = self.scopes.len();
        self.push_loop(depth, depth, false);
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

    fn compile_do_while(&mut self, body: &Statement<'_>, test: &Expression<'_>) -> R<()> {
        let l_top = self.code_len();
        let depth = self.scopes.len();
        self.push_loop(depth, depth, false);
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

    fn compile_for(&mut self, s: &ForStatement<'_>) -> R<()> {
        let lexical_head = s
            .init
            .as_ref()
            .is_some_and(ForStatementInit::is_lexical_declaration);
        if lexical_head {
            // A `for (let ...)` binding is fresh per iteration only so a closure
            // can capture each iteration's value. estrogen uses one binding, so if
            // the loop makes a closure the two models differ; decline instead of
            // miscompiling.
            let captured = s.test.as_ref().is_some_and(|e| has_closure_expr(e))
                || s.update.as_ref().is_some_and(|e| has_closure_expr(e))
                || has_closure_stmt(&s.body);
            if captured {
                return Err(Unsupported(
                    "for-loop lexical binding may be captured by a closure",
                ));
            }
            self.open_scope();
        }
        let break_depth = self.scopes.len() - usize::from(lexical_head);

        match &s.init {
            None => {}
            Some(ForStatementInit::VariableDeclaration(decl)) => {
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
        self.push_loop(break_depth, continue_depth, false);
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

    fn compile_switch(&mut self, s: &SwitchStatement<'_>) -> R<()> {
        // A lexical declaration in a case needs the switch block-scope model,
        // which is not ported yet; decline such switches.
        for case in &s.cases {
            if block_needs_scope(&case.consequent) {
                return Err(Unsupported("lexical declaration in a switch case"));
            }
        }
        // The discriminant lives in a temp slot, not on the operand stack, so a
        // `break`/`continue` out of the switch never leaves a stray value.
        self.open_scope();
        let temp = self.add_local("*switch*");
        self.emit1("DECLARE_SLOT", (temp as i64) << 2);
        self.compile_expr(&s.discriminant)?;
        self.emit1("INIT_SLOT", pack_addr(0, temp));

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
        self.push_loop(depth, depth, true);
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
                if !tpl.expressions.is_empty() || tpl.quasis.len() != 1 {
                    return Err(Unsupported("template literal with substitutions"));
                }
                let cooked = tpl.quasis[0]
                    .value
                    .cooked
                    .and_then(|c| c.as_str())
                    .ok_or(Unsupported("template with no cooked value"))?;
                let i = self.consts.add_str(cooked);
                self.emit1("PUSH_CONST", i as i64);
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
                if func.r#async || func.generator {
                    return Err(Unsupported("async/generator function expression"));
                }
                if func.id.is_some() {
                    return Err(Unsupported("named function expression"));
                }
                let fb = func
                    .body
                    .as_ref()
                    .ok_or(Unsupported("function without a body"))?;
                let idx = self.compile_function(
                    &func.params,
                    FnBody::Block(&fb.statements),
                    None,
                    false,
                )?;
                self.emit1("MAKE_FUNC", idx as i64);
            }
            Expression::ArrowFunctionExpression(arrow) => {
                if arrow.r#async {
                    return Err(Unsupported("async arrow function"));
                }
                let idx = match &arrow.body {
                    ArrowFunctionBody::FunctionBody(fb) => self.compile_function(
                        &arrow.params,
                        FnBody::Block(&fb.statements),
                        None,
                        true,
                    )?,
                    _ => {
                        let e = arrow.get_expression().ok_or(Unsupported("arrow body"))?;
                        self.compile_function(&arrow.params, FnBody::Expr(e), None, true)?
                    }
                };
                self.emit1("MAKE_FUNC", idx as i64);
            }
            Expression::StaticMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional member access"));
                }
                self.compile_expr(&m.object)?;
                let i = self.consts.add_str(m.property.name.as_str());
                self.emit1("GET_PROP", i as i64);
            }
            Expression::ComputedMemberExpression(m) => {
                if m.optional {
                    return Err(Unsupported("optional member access"));
                }
                self.compile_expr(&m.object)?;
                self.compile_expr(&m.expression)?;
                self.emit0("GET_PROP_DYN");
            }
            Expression::CallExpression(call) => self.compile_call(call)?,
            Expression::NewExpression(new) => {
                self.compile_expr(&new.callee)?;
                let argc = self.compile_args(&new.arguments)?;
                self.emit1("NEW", argc as i64);
            }
            Expression::ArrayExpression(arr) => {
                self.emit0("ARR_EMPTY");
                for el in &arr.elements {
                    match el {
                        ArrayExpressionElement::Elision(_) => {
                            return Err(Unsupported("array hole"))
                        }
                        ArrayExpressionElement::SpreadElement(_) => {
                            return Err(Unsupported("array spread"))
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
                        ObjectPropertyKind::SpreadProperty(_) => {
                            return Err(Unsupported("object spread"))
                        }
                    };
                    if p.computed || p.method || p.kind != PropertyKind::Init {
                        return Err(Unsupported("computed/method/accessor property"));
                    }
                    let key = static_key(&p.key).ok_or(Unsupported("non-static object key"))?;
                    self.compile_expr(&p.value)?;
                    let i = self.consts.add_str(&key);
                    self.emit1("OBJ_SET", i as i64);
                }
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
            UnaryOperator::Delete => return Err(Unsupported("delete")),
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
                    self.compile_expr(&node.right)?;
                    self.emit_store_name(id.name.as_str());
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
                        MemberExpression::PrivateFieldExpression(_) => {
                            return Err(Unsupported("private field assignment"))
                        }
                    }
                }
            },
            AssignmentOperator::LogicalAnd
            | AssignmentOperator::LogicalOr
            | AssignmentOperator::LogicalNullish => {
                let id = match &node.left {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => id,
                    _ => return Err(Unsupported("logical assignment to this target")),
                };
                let short = match node.operator {
                    AssignmentOperator::LogicalAnd => "JMP_IF_FALSY_KEEP",
                    AssignmentOperator::LogicalOr => "JMP_IF_TRUTHY_KEEP",
                    _ => "JMP_IF_NOT_NULLISH_KEEP",
                };
                self.emit_load_name(id.name.as_str())?;
                let to_end = self.emit_jump(short);
                self.emit0("POP");
                self.compile_expr(&node.right)?;
                self.emit_store_name(id.name.as_str());
                self.patch_here(to_end);
            }
            other => {
                let bin = compound_binary_op(other)?;
                let id = match &node.left {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => id,
                    _ => return Err(Unsupported("compound assignment to this target")),
                };
                self.emit_load_name(id.name.as_str())?;
                self.compile_expr(&node.right)?;
                self.emit0(bin);
                self.emit_store_name(id.name.as_str());
            }
        }
        Ok(())
    }

    fn compile_update(&mut self, node: &oxc_ast::ast::UpdateExpression<'_>) -> R<()> {
        let id = match &node.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => id,
            _ => return Err(Unsupported("update on a member target")),
        };
        let name = id.name.as_str().to_string();
        let delta = match node.operator {
            UpdateOperator::Increment => 1.0,
            UpdateOperator::Decrement => -1.0,
        };
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

    fn compile_call(&mut self, call: &oxc_ast::ast::CallExpression<'_>) -> R<()> {
        if call.optional {
            return Err(Unsupported("optional call"));
        }
        match &call.callee {
            Expression::StaticMemberExpression(m) if !m.optional => {
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                let i = self.consts.add_str(m.property.name.as_str());
                self.emit1("GET_PROP", i as i64);
                let argc = self.compile_args(&call.arguments)?;
                self.emit1("CALL_METHOD", argc as i64);
            }
            Expression::ComputedMemberExpression(m) if !m.optional => {
                self.compile_expr(&m.object)?;
                self.emit0("DUP");
                self.compile_expr(&m.expression)?;
                self.emit0("GET_PROP_DYN");
                let argc = self.compile_args(&call.arguments)?;
                self.emit1("CALL_METHOD", argc as i64);
            }
            callee => {
                self.compile_expr(callee)?;
                let argc = self.compile_args(&call.arguments)?;
                self.emit1("CALL", argc as i64);
            }
        }
        Ok(())
    }

    fn compile_args(&mut self, args: &[Argument<'_>]) -> R<usize> {
        for a in args {
            if let Argument::SpreadElement(_) = a {
                return Err(Unsupported("call spread argument"));
            }
            let e = a.as_expression().ok_or(Unsupported("call argument"))?;
            self.compile_expr(e)?;
        }
        Ok(args.len())
    }

    // ---- name resolution ----

    fn add_local(&mut self, name: &str) -> usize {
        let scope = self.scopes.last_mut().expect("a scope is open");
        let idx = scope.names.len();
        scope.names.push(name.to_string());
        idx
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
                self.reject_arguments(name)?;
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
                self.reject_arguments(name)?;
                let i = self.consts.add_str(name);
                self.emit1("TYPEOF_GLOBAL", i as i64);
            }
        }
        Ok(())
    }

    /// `arguments` inside a function is the arguments object, which estrogen does
    /// not model, so a function that reads it falls back. At the module top level
    /// `arguments` is a genuine free name (a `ReferenceError`), so it is left as a
    /// global, matching native.
    fn reject_arguments(&self, name: &str) -> R<()> {
        if name == "arguments" && !self.fns.last().expect("frame").is_module_main {
            return Err(Unsupported("arguments object"));
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

    fn push_loop(&mut self, break_depth: usize, continue_depth: usize, is_switch: bool) {
        self.fns.last_mut().expect("frame").loops.push(LoopState {
            break_sites: Vec::new(),
            continue_sites: Vec::new(),
            break_depth,
            continue_depth,
            is_switch,
        });
    }

    fn pop_loop(&mut self) -> LoopState {
        self.fns
            .last_mut()
            .expect("frame")
            .loops
            .pop()
            .expect("open loop")
    }

    fn emit_break(&mut self) -> R<()> {
        let loops = &self.fns.last().expect("frame").loops;
        let idx = loops
            .len()
            .checked_sub(1)
            .ok_or(Unsupported("break outside a loop or switch"))?;
        let target = loops[idx].break_depth;
        self.unwind_scopes(target);
        let site = self.emit_jump("JMP");
        self.fns.last_mut().expect("frame").loops[idx]
            .break_sites
            .push(site);
        Ok(())
    }

    fn emit_continue(&mut self) -> R<()> {
        let loops = &self.fns.last().expect("frame").loops;
        let idx = loops
            .iter()
            .rposition(|l| !l.is_switch)
            .ok_or(Unsupported("continue outside a loop"))?;
        let target = loops[idx].continue_depth;
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

/// The plain identifier names of a parameter list, or [`Unsupported`] for rest,
/// default, or destructured parameters.
fn parse_params(params: &FormalParameters<'_>) -> R<Vec<String>> {
    if params.rest.is_some() {
        return Err(Unsupported("rest parameter"));
    }
    if params.items.len() > 0xff {
        return Err(Unsupported("too many parameters"));
    }
    let mut names = Vec::with_capacity(params.items.len());
    for item in &params.items {
        if item.initializer.is_some() {
            return Err(Unsupported("default parameter"));
        }
        names.push(binding_identifier(&item.pattern)?.name.as_str().to_string());
    }
    Ok(names)
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
