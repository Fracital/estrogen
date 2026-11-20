//! OXC AST to stack bytecode, for the subset estrogen's VM can compile today.
//!
//! The compiler walks an OXC `Program` and emits the flat opcode array the
//! runtime runs. It does not reprint source: it produces a program object that
//! replaces the whole chunk (see [`super`]). A construct outside the subset
//! returns [`Unsupported`], and the caller falls back to the literal-pool and
//! name passes, the same way abulxt's shield degrades a chunk to `@no-vm`.
//!
//! The subset: a module body of expression statements and `let`/`const`
//! declarations with simple identifier bindings; expressions over literals,
//! identifiers (locals resolve to slots, free names to globals), member access,
//! calls (including method calls), `new`, unary and binary operators, logical
//! and conditional operators, array and object literals, and sequence
//! expressions. Anything else (functions, classes, control flow, `import`/
//! `export`, `var`, destructuring, assignment, update, templates with
//! substitutions, optional chaining, spread, `await`) falls back.
//!
//! Locals are slot-based exactly as the runtime expects: every top-level
//! `let`/`const` gets a slot in the main frame's base scope (depth 0), is
//! declared as a TDZ cell at entry, and is initialized at its declarator. A free
//! name compiles to a `*_GLOBAL` op. This mirrors the shield compiler's
//! slot/global split.

use oxc_ast::ast::{
    Argument, ArrayExpressionElement, BinaryOperator, BindingPattern, Expression, LogicalOperator,
    ObjectPropertyKind, Program, PropertyKey, PropertyKind, Statement, UnaryOperator,
    VariableDeclarationKind,
};

use super::layout::Layout;
use super::program::{ConstPool, FunctionDef, Program as VmProgram};

/// A construct estrogen's VM cannot compile yet. The caller treats it as a signal
/// to fall back, not as a build failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

type R<T> = Result<T, Unsupported>;

/// Compile a parsed module into a VM program, or report the first construct
/// outside the subset. `seed` keys the opcode layout.
pub fn compile_program(program: &Program<'_>, seed: &str) -> R<VmProgram> {
    let mut c = Compiler {
        layout: Layout::build(seed),
        consts: ConstPool::default(),
        code: Vec::new(),
        locals: Vec::new(),
    };
    c.compile_main(program)?;
    let main = FunctionDef {
        code: c.code,
        name: None,
        is_async: false,
        is_arrow: false,
        num_params: 0,
    };
    Ok(VmProgram {
        layout: c.layout,
        consts: c.consts,
        functions: vec![main],
        main_index: 0,
    })
}

struct Compiler {
    layout: Layout,
    consts: ConstPool,
    code: Vec<i64>,
    /// Top-level lexical binding names, indexed by slot.
    locals: Vec<String>,
}

impl Compiler {
    fn compile_main(&mut self, program: &Program<'_>) -> R<()> {
        // Pass 0: assign a slot to every top-level let/const name, and refuse any
        // top-level construct outside the subset so the whole chunk falls back.
        for stmt in &program.body {
            match stmt {
                Statement::ExpressionStatement(_) | Statement::EmptyStatement(_) => {}
                Statement::VariableDeclaration(decl) => {
                    let kind = match decl.kind {
                        VariableDeclarationKind::Let | VariableDeclarationKind::Const => decl.kind,
                        _ => return Err(Unsupported("var/using declaration")),
                    };
                    let _ = kind;
                    for d in &decl.declarations {
                        match &d.id {
                            BindingPattern::BindingIdentifier(id) => {
                                let name = id.name.as_str().to_string();
                                if !self.locals.iter().any(|n| n == &name) {
                                    self.locals.push(name);
                                }
                            }
                            _ => return Err(Unsupported("destructuring binding")),
                        }
                    }
                }
                _ => return Err(Unsupported("top-level statement outside the subset")),
            }
        }

        // Pass 1: TDZ cells for every top-level lexical, so a forward reference
        // throws like native rather than reading a stale slot.
        for stmt in &program.body {
            if let Statement::VariableDeclaration(decl) = stmt {
                let kind: i64 = match decl.kind {
                    VariableDeclarationKind::Let => 1,
                    VariableDeclarationKind::Const => 2,
                    _ => continue,
                };
                for d in &decl.declarations {
                    if let BindingPattern::BindingIdentifier(id) = &d.id {
                        let slot = self.slot_of(id.name.as_str());
                        self.emit1("DECLARE_SLOT", ((slot as i64) << 2) | kind);
                    }
                }
            }
        }

        // Pass 2: the statements in order.
        for stmt in &program.body {
            self.compile_statement(stmt)?;
        }
        self.emit0("HALT");
        Ok(())
    }

    fn compile_statement(&mut self, stmt: &Statement<'_>) -> R<()> {
        match stmt {
            Statement::EmptyStatement(_) => Ok(()),
            Statement::ExpressionStatement(stmt) => {
                self.compile_expr(&stmt.expression)?;
                // Top-level expression statements set the module completion value,
                // which `run()` returns. The last one wins.
                self.emit0("SET_COMPLETION");
                Ok(())
            }
            Statement::VariableDeclaration(decl) => {
                for d in &decl.declarations {
                    let id = match &d.id {
                        BindingPattern::BindingIdentifier(id) => id,
                        _ => return Err(Unsupported("destructuring binding")),
                    };
                    let slot = self.slot_of(id.name.as_str());
                    match &d.init {
                        Some(init) => self.compile_expr(init)?,
                        None => self.emit0("PUSH_UNDEF"),
                    }
                    self.emit1("INIT_SLOT", pack_addr(0, slot));
                }
                Ok(())
            }
            _ => Err(Unsupported("statement outside the subset")),
        }
    }

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
            Expression::Identifier(id) => self.emit_load_name(id.name.as_str()),
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
                    self.emit_typeof_name(id.name.as_str());
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

    /// Compile each call argument in order, returning the count. A spread
    /// argument falls back.
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
    fn slot_of(&self, name: &str) -> usize {
        self.locals
            .iter()
            .position(|n| n == name)
            .expect("slot_of called for an undeclared local")
    }

    fn resolve(&self, name: &str) -> Option<usize> {
        self.locals.iter().position(|n| n == name)
    }

    fn emit_load_name(&mut self, name: &str) {
        match self.resolve(name) {
            Some(slot) => self.emit1("LOAD_SLOT", pack_addr(0, slot)),
            None => {
                let i = self.consts.add_str(name);
                self.emit1("LOAD_GLOBAL", i as i64);
            }
        }
    }

    fn emit_typeof_name(&mut self, name: &str) {
        match self.resolve(name) {
            Some(slot) => self.emit1("TYPEOF_SLOT", pack_addr(0, slot)),
            None => {
                let i = self.consts.add_str(name);
                self.emit1("TYPEOF_GLOBAL", i as i64);
            }
        }
    }

    // ---- emit ----
    fn emit0(&mut self, name: &str) {
        debug_assert_eq!(super::opcodes::arity(name), 0, "{name} is not nullary");
        self.code.push(self.layout.byte_of(name) as i64);
    }

    fn emit1(&mut self, name: &str, operand: i64) {
        debug_assert_eq!(super::opcodes::arity(name), 1, "{name} takes no operand");
        self.code.push(self.layout.byte_of(name) as i64);
        self.code.push(operand);
    }

    /// Emit a jump opcode with a placeholder target, and return the code index of
    /// the operand to patch once the target is known.
    fn emit_jump(&mut self, name: &str) -> usize {
        self.code.push(self.layout.byte_of(name) as i64);
        let slot = self.code.len();
        self.code.push(0);
        slot
    }

    /// Patch a jump's placeholder operand to the current end of the code array.
    fn patch_here(&mut self, slot: usize) {
        self.code[slot] = self.code.len() as i64;
    }
}

/// Pack a slot address the way the runtime unpacks it: `depth << 16 | index`.
fn pack_addr(depth: usize, index: usize) -> i64 {
    ((depth as i64) << 16) | (index as i64)
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

/// The string key of a non-computed object property, or `None` for a key estrogen
/// does not lower (computed, private, a complex expression).
fn static_key(key: &PropertyKey<'_>) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str().to_string()),
        PropertyKey::StringLiteral(s) => s.value.as_str().map(|v| v.to_string()),
        _ => None,
    }
}
