//! Opcode names and arities.
//!
//! The names and their order match abulxt's shield `opcodes.ts` exactly (the
//! insertion order of its `ARITY` object). The order is load-bearing:
//! [`super::layout::build_layout`] assigns each name a byte by shuffling a slot
//! pool in this order, so a different order would produce a different layout
//! than the shield runtime expects. The arities match the runtime's dispatch
//! (`arity` operands follow each opcode byte in the flat code array).
//!
//! estrogen's compiler emits a subset of these. The full list is kept so the
//! per-build layout is a bijection over the same name set the shield uses, which
//! keeps the on-disk program shape one contract with the runtime.

/// Every opcode name, in the shield's `ARITY` insertion order.
pub const OP_NAMES: &[&str] = &[
    "HALT",
    "NOP",
    "POP",
    "DUP",
    "DUP2",
    "SWAP",
    "ROTL3",
    "PUSH_CONST",
    "PUSH_UNDEF",
    "PUSH_NULL",
    "PUSH_TRUE",
    "PUSH_FALSE",
    "PUSH_THIS",
    "LOAD_VAR",
    "STORE_VAR",
    "INIT_VAR",
    "DECLARE_VAR",
    "DECLARE_LET",
    "DECLARE_CONST",
    "DEF_FUNC",
    "TYPEOF_VAR",
    "LOAD_ARG",
    "LOAD_ARGS_FROM",
    "LOAD_SLOT",
    "STORE_SLOT",
    "INIT_SLOT",
    "TYPEOF_SLOT",
    "DECLARE_SLOT",
    "DEF_FUNC_SLOT",
    "LOAD_GLOBAL",
    "STORE_GLOBAL",
    "TYPEOF_GLOBAL",
    "IMPORT_SLOT",
    "BIND_PARAM",
    "LOAD_SLOT_PROP",
    "LOAD_GLOBAL_PROP",
    "LOAD_SLOT_DUP_PROP",
    "LOAD_GLOBAL_DUP_PROP",
    "OBJ_SET_CONST",
    "DECLARE_SLOTS",
    "ENTER_SCOPE",
    "EXIT_SCOPE",
    "ADD",
    "SUB",
    "MUL",
    "DIV",
    "MOD",
    "EXP",
    "EQ",
    "NEQ",
    "SEQ",
    "SNEQ",
    "LT",
    "LTE",
    "GT",
    "GTE",
    "BAND",
    "BOR",
    "BXOR",
    "SHL",
    "SHR",
    "USHR",
    "NOT",
    "NEG",
    "POS",
    "BNOT",
    "TYPEOF",
    "INSTANCEOF",
    "IN",
    "GET_PROP",
    "GET_PROP_DYN",
    "SET_PROP",
    "SET_PROP_DYN",
    "DELETE_PROP",
    "DELETE_PROP_DYN",
    "JMP",
    "JMP_IF_FALSE",
    "JMP_IF_TRUE",
    "JMP_IF_FALSY_KEEP",
    "JMP_IF_TRUTHY_KEEP",
    "JMP_IF_NOT_NULLISH_KEEP",
    "OPT_SHORT",
    "OPT_SHORT_CALL",
    "CALL",
    "CALL_METHOD",
    "NEW",
    "CALL_APPLY",
    "CALL_METHOD_APPLY",
    "NEW_APPLY",
    "MAKE_FUNC",
    "RETURN",
    "RETURN_UNDEF",
    "ARR_EMPTY",
    "ARR_APPEND",
    "ARR_SPREAD",
    "OBJ_EMPTY",
    "OBJ_SET",
    "OBJ_SET_DYN",
    "OBJ_SPREAD",
    "OBJ_DEFINE_GETTER",
    "OBJ_DEFINE_SETTER",
    "OBJ_REST",
    "OBJ_REST_DYN",
    "GET_ITERATOR",
    "FOR_ITER",
    "ITER_NEXT_PUSH",
    "ITER_REST_PUSH",
    "FOR_IN_KEYS",
    "THROW",
    "RETHROW",
    "PUSH_HANDLER",
    "PUSH_FINALLY",
    "POP_HANDLER",
    "PUSH_COMPLETION_NORMAL",
    "END_FINALLY",
    "ABRUPT_JUMP",
    "AWAIT",
    "BUILD_CLASS",
    "LOAD_NEW_TARGET",
    "SUPER_GET",
    "SUPER_GET_DYN",
    "SUPER_SET",
    "SUPER_SET_DYN",
    "SUPER_CALL",
    "SUPER_CALL_APPLY",
    "GET_PRIVATE",
    "SET_PRIVATE",
    "HAS_PRIVATE",
    "INIT_PRIVATE",
    "MODULE_INIT",
    "IMPORT_BINDING",
    "EXPORT_BINDING",
    "DYNAMIC_IMPORT",
    "IMPORT_META",
    "AWAIT_TOP",
    "SET_COMPLETION",
    "LOAD_ARGUMENTS",
    "YIELD",
    "YIELD_STAR",
    "OBJ_DEFINE_GETTER_DYN",
    "OBJ_DEFINE_SETTER_DYN",
    "BUILD_CLASS_DYN",
];

/// How many operand numbers follow this opcode's byte in the flat code array.
/// Every opcode is either nullary or takes one operand. Unknown names are a
/// compiler bug, so this panics rather than guessing.
pub fn arity(name: &str) -> usize {
    match name {
        // one operand: a const index, a packed slot address, a jump target, or
        // an argument count.
        "PUSH_CONST"
        | "LOAD_VAR"
        | "STORE_VAR"
        | "INIT_VAR"
        | "DECLARE_VAR"
        | "DECLARE_LET"
        | "DECLARE_CONST"
        | "DEF_FUNC"
        | "TYPEOF_VAR"
        | "LOAD_ARG"
        | "LOAD_ARGS_FROM"
        | "LOAD_SLOT"
        | "STORE_SLOT"
        | "INIT_SLOT"
        | "TYPEOF_SLOT"
        | "DECLARE_SLOT"
        | "DEF_FUNC_SLOT"
        | "LOAD_GLOBAL"
        | "STORE_GLOBAL"
        | "TYPEOF_GLOBAL"
        | "IMPORT_SLOT"
        | "BIND_PARAM"
        | "LOAD_SLOT_PROP"
        | "LOAD_GLOBAL_PROP"
        | "LOAD_SLOT_DUP_PROP"
        | "LOAD_GLOBAL_DUP_PROP"
        | "OBJ_SET_CONST"
        | "DECLARE_SLOTS"
        | "GET_PROP"
        | "SET_PROP"
        | "DELETE_PROP"
        | "JMP"
        | "JMP_IF_FALSE"
        | "JMP_IF_TRUE"
        | "JMP_IF_FALSY_KEEP"
        | "JMP_IF_TRUTHY_KEEP"
        | "JMP_IF_NOT_NULLISH_KEEP"
        | "OPT_SHORT"
        | "OPT_SHORT_CALL"
        | "CALL"
        | "CALL_METHOD"
        | "NEW"
        | "MAKE_FUNC"
        | "OBJ_SET"
        | "OBJ_DEFINE_GETTER"
        | "OBJ_DEFINE_SETTER"
        | "OBJ_REST"
        | "FOR_ITER"
        | "PUSH_HANDLER"
        | "PUSH_FINALLY"
        | "END_FINALLY"
        | "ABRUPT_JUMP"
        | "BUILD_CLASS"
        | "BUILD_CLASS_DYN"
        | "SUPER_GET"
        | "SUPER_SET"
        | "SUPER_CALL"
        | "GET_PRIVATE"
        | "SET_PRIVATE"
        | "HAS_PRIVATE"
        | "INIT_PRIVATE"
        | "MODULE_INIT"
        | "IMPORT_BINDING"
        | "EXPORT_BINDING" => 1,
        // nullary.
        "HALT"
        | "NOP"
        | "POP"
        | "DUP"
        | "DUP2"
        | "SWAP"
        | "ROTL3"
        | "PUSH_UNDEF"
        | "PUSH_NULL"
        | "PUSH_TRUE"
        | "PUSH_FALSE"
        | "PUSH_THIS"
        | "ENTER_SCOPE"
        | "EXIT_SCOPE"
        | "ADD"
        | "SUB"
        | "MUL"
        | "DIV"
        | "MOD"
        | "EXP"
        | "EQ"
        | "NEQ"
        | "SEQ"
        | "SNEQ"
        | "LT"
        | "LTE"
        | "GT"
        | "GTE"
        | "BAND"
        | "BOR"
        | "BXOR"
        | "SHL"
        | "SHR"
        | "USHR"
        | "NOT"
        | "NEG"
        | "POS"
        | "BNOT"
        | "TYPEOF"
        | "INSTANCEOF"
        | "IN"
        | "GET_PROP_DYN"
        | "SET_PROP_DYN"
        | "DELETE_PROP_DYN"
        | "CALL_APPLY"
        | "CALL_METHOD_APPLY"
        | "NEW_APPLY"
        | "RETURN"
        | "RETURN_UNDEF"
        | "ARR_EMPTY"
        | "ARR_APPEND"
        | "ARR_SPREAD"
        | "OBJ_EMPTY"
        | "OBJ_SET_DYN"
        | "OBJ_SPREAD"
        | "OBJ_REST_DYN"
        | "GET_ITERATOR"
        | "ITER_NEXT_PUSH"
        | "ITER_REST_PUSH"
        | "FOR_IN_KEYS"
        | "THROW"
        | "RETHROW"
        | "POP_HANDLER"
        | "PUSH_COMPLETION_NORMAL"
        | "AWAIT"
        | "LOAD_NEW_TARGET"
        | "SUPER_GET_DYN"
        | "SUPER_SET_DYN"
        | "SUPER_CALL_APPLY"
        | "DYNAMIC_IMPORT"
        | "IMPORT_META"
        | "AWAIT_TOP"
        | "SET_COMPLETION"
        | "LOAD_ARGUMENTS"
        | "YIELD"
        | "YIELD_STAR"
        | "OBJ_DEFINE_GETTER_DYN"
        | "OBJ_DEFINE_SETTER_DYN" => 0,
        other => panic!("estrogen vm: unknown opcode {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_names_fit_the_table_and_are_unique() {
        assert!(
            OP_NAMES.len() <= 256,
            "too many opcodes for a 256-slot table"
        );
        let mut seen = std::collections::HashSet::new();
        for name in OP_NAMES {
            assert!(seen.insert(*name), "duplicate opcode name {name}");
            // every name has a defined arity.
            let _ = arity(name);
        }
    }
}
