use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::{bail, Result};

use crate::lir::{
    BinaryOp, ConstValue, Expr, Function, Label, LocalId, MathBinary, MathUnary, Stmt, UnaryOp,
};
use crate::lir_structure::{self, StructuredStmt};

pub(crate) fn emit_function(function: &Function) -> Result<String> {
    let names = NameTable::new(function);
    let function_name = sanitize_ident(&function.name);
    let start = Instant::now();
    let structured = lir_structure::structure(function)?;
    if std::env::var_os("MIR_LIFT_TIMING").is_some() {
        eprintln!("mir-lift timing: {} structure {:?}", function.name, start.elapsed());
    }
    let helper_params = structured
        .helpers
        .iter()
        .map(|helper| (helper.label, helper.params.clone()))
        .collect::<HashMap<_, _>>();
    let captures_outputs = function
        .blocks
        .iter()
        .flat_map(|block| block.stmts.iter())
        .any(|stmt| matches!(stmt, Stmt::Capture { .. }));
    let mut out = String::new();
    let cx = EmitCx {
        names: &names,
        helper_params,
        optional_helper_live_ins: &structured.facts.optional_helper_live_ins,
        helper_prefix: &function_name,
        captures_outputs,
    };

    if !structured.facts.optional_helper_live_ins.is_empty() || captures_outputs {
        writeln!(out, "_LIR_UNDEF = object()")?;
        writeln!(out)?;
    }

    for helper in &structured.helpers {
        writeln!(
            out,
            "def {}({}):",
            helper_name(&function_name, helper.label),
            helper_params_signature(&names, helper.params.as_slice(), captures_outputs)
        )?;
        let mut defined = helper.params.iter().copied().collect::<HashSet<_>>();
        emit_structured_body(&mut out, &helper.body, &cx, 1, &mut defined)?;
        writeln!(out)?;
    }

    let entry_name = entry_helper_name(&function_name);
    writeln!(
        out,
        "def {}({}):",
        entry_name,
        helper_params_signature(&names, function.params.as_slice(), captures_outputs)
    )?;
    let mut defined = function.params.iter().copied().collect::<HashSet<_>>();
    emit_structured_body(&mut out, &structured.body, &cx, 1, &mut defined)?;
    writeln!(out)?;

    writeln!(out, "def {}({}):", function_name, names.params(function))?;
    if captures_outputs {
        writeln!(out, "    _lir_outputs = {{}}")?;
    }
    writeln!(out, "    _lir_target = {}", entry_name)?;
    writeln!(
        out,
        "    _lir_args = {}",
        entry_args_tuple(names.params(function), captures_outputs)
    )?;
    writeln!(out, "    while True:")?;
    writeln!(out, "        _lir_result = _lir_target(*_lir_args)")?;
    writeln!(out, "        if _lir_result[0] == \"return\":")?;
    writeln!(out, "            return _lir_result[1]")?;
    writeln!(out, "        _lir_target = _lir_result[1]")?;
    writeln!(out, "        _lir_args = _lir_result[2]")?;

    Ok(out)
}

fn emit_stmt(out: &mut String, stmt: &Stmt, names: &NameTable, indent: usize) -> Result<()> {
    match stmt {
        Stmt::Assign { dst, value } => {
            writeln!(out, "{}{} = {}", pad(indent), names.local(*dst), expr(value, names)?)?;
        }
        Stmt::Capture { key, value } => {
            emit_capture(out, key, value, names, indent)?;
        }
        Stmt::Expr(value) => {
            writeln!(out, "{}{}", pad(indent), expr(value, names)?)?;
        }
        Stmt::Unsupported { text, .. } => {
            bail!("unsupported LIR statement: {text}");
        }
    }
    Ok(())
}

fn emit_capture(
    out: &mut String,
    key: &str,
    value: &Expr,
    names: &NameTable,
    indent: usize,
) -> Result<()> {
    let locals = expr_local_ids(value);
    if locals.is_empty() {
        writeln!(out, "{}_lir_outputs[{key:?}] = {}", pad(indent), expr(value, names)?)?;
        return Ok(());
    }

    let guard = locals
        .iter()
        .map(|local| format!("{} is not _LIR_UNDEF", names.local(*local)))
        .collect::<Vec<_>>()
        .join(" and ");
    writeln!(out, "{}if {guard}:", pad(indent))?;
    writeln!(out, "{}_lir_outputs[{key:?}] = {}", pad(indent + 1), expr(value, names)?)?;
    writeln!(out, "{}else:", pad(indent))?;
    writeln!(out, "{}_lir_outputs.pop({key:?}, None)", pad(indent + 1))?;
    Ok(())
}

fn expr(value: &Expr, names: &NameTable) -> Result<String> {
    Ok(match value {
        Expr::Local(local) => names.local(*local),
        Expr::Const(ConstValue::Bool(true)) => "True".to_owned(),
        Expr::Const(ConstValue::Bool(false)) => "False".to_owned(),
        Expr::Const(ConstValue::Int(value)) => value.to_string(),
        Expr::Const(ConstValue::Real(value)) => value.to_string(),
        Expr::Const(ConstValue::Str(value)) => format!("{value:?}"),
        Expr::Const(ConstValue::None) => "None".to_owned(),
        Expr::Unary { op, arg } => unary_expr(*op, expr(arg, names)?),
        Expr::Binary { op, lhs, rhs } => binary_expr(*op, expr(lhs, names)?, expr(rhs, names)?),
        Expr::Call { target, args } => {
            let args =
                args.iter().map(|arg| expr(arg, names)).collect::<Result<Vec<_>>>()?.join(", ");
            if args.is_empty() {
                format!("mir_call({target:?})")
            } else {
                format!("mir_call({target:?}, {args})")
            }
        }
        Expr::Unsupported { text, .. } => bail!("unsupported LIR expression: {text}"),
    })
}

fn emit_structured_body(
    out: &mut String,
    body: &[StructuredStmt],
    cx: &EmitCx<'_>,
    indent: usize,
    defined: &mut HashSet<LocalId>,
) -> Result<()> {
    if body.is_empty() {
        writeln!(out, "{}raise RuntimeError(\"empty LIR body\")", pad(indent))?;
        return Ok(());
    }

    for stmt in body {
        emit_structured_stmt(out, stmt, cx, indent, defined)?;
    }
    Ok(())
}

fn emit_structured_stmt(
    out: &mut String,
    stmt: &StructuredStmt,
    cx: &EmitCx<'_>,
    indent: usize,
    defined: &mut HashSet<LocalId>,
) -> Result<()> {
    match stmt {
        StructuredStmt::Stmt(stmt) => {
            emit_stmt(out, stmt, cx.names, indent)?;
            mark_stmt_defs(stmt, defined);
        }
        StructuredStmt::If { cond, then_body, else_body } => {
            writeln!(out, "{}if {}:", pad(indent), expr(cond, cx.names)?)?;
            let mut then_defined = defined.clone();
            emit_structured_body(out, then_body, cx, indent + 1, &mut then_defined)?;
            writeln!(out, "{}else:", pad(indent))?;
            let mut else_defined = defined.clone();
            emit_structured_body(out, else_body, cx, indent + 1, &mut else_defined)?;
            then_defined.retain(|local| else_defined.contains(local));
            *defined = then_defined;
        }
        StructuredStmt::CallHelper(label) => {
            let args = helper_args_tuple(*label, cx, defined)?;
            writeln!(
                out,
                "{}return (\"call\", {}, {args})",
                pad(indent),
                helper_name(cx.helper_prefix, *label)
            )?;
        }
        StructuredStmt::Return(values) => {
            emit_return(out, values, cx, indent)?;
        }
        StructuredStmt::Raise(message) => {
            writeln!(out, "{}raise RuntimeError({message:?})", pad(indent))?;
        }
    }
    Ok(())
}

fn helper_args_tuple(label: Label, cx: &EmitCx<'_>, defined: &HashSet<LocalId>) -> Result<String> {
    let mut args = if cx.captures_outputs { vec!["_lir_outputs".to_owned()] } else { Vec::new() };
    args.extend(
        cx.helper_params
            .get(&label)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|arg| -> Result<String> {
                if !defined.contains(&arg) && !cx.optional_helper_live_ins.contains(&(label, arg)) {
                    bail!(
                        "structured LIR helper {} requires {} before it is defined",
                        helper_name(cx.helper_prefix, label),
                        cx.names.local(arg)
                    );
                }
                if defined.contains(&arg) {
                    Ok(cx.names.local(arg))
                } else {
                    Ok("_LIR_UNDEF".to_owned())
                }
            })
            .collect::<Result<Vec<_>>>()?,
    );
    let args = args.join(", ");

    if args.is_empty() {
        Ok("()".to_owned())
    } else {
        Ok(format!("({args},)"))
    }
}

fn emit_return(
    out: &mut String,
    values: &[crate::lir::ReturnValue],
    cx: &EmitCx<'_>,
    indent: usize,
) -> Result<()> {
    if cx.optional_helper_live_ins.is_empty() {
        if cx.captures_outputs {
            writeln!(out, "{}_lir_return = dict(_lir_outputs)", pad(indent))?;
            for value in values {
                match value {
                    crate::lir::ReturnValue::Named { key, value } => {
                        writeln!(
                            out,
                            "{}_lir_return[{key:?}] = {}",
                            pad(indent),
                            expr(value, cx.names)?
                        )?;
                    }
                }
            }
            writeln!(out, "{}return (\"return\", _lir_return)", pad(indent))?;
            return Ok(());
        }
        let entries = values
            .iter()
            .map(|value| match value {
                crate::lir::ReturnValue::Named { key, value } => {
                    Ok(format!("{key:?}: {}", expr(value, cx.names)?))
                }
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        writeln!(out, "{}return (\"return\", {{{entries}}})", pad(indent))?;
        return Ok(());
    }

    if cx.captures_outputs {
        writeln!(out, "{}_lir_return = dict(_lir_outputs)", pad(indent))?;
    } else {
        writeln!(out, "{}_lir_return = {{}}", pad(indent))?;
    }
    for value in values {
        match value {
            crate::lir::ReturnValue::Named { key, value } => {
                let locals = expr_local_ids(value);
                if locals.is_empty() {
                    writeln!(
                        out,
                        "{}_lir_return[{key:?}] = {}",
                        pad(indent),
                        expr(value, cx.names)?
                    )?;
                } else {
                    let guard = locals
                        .iter()
                        .map(|local| format!("{} is not _LIR_UNDEF", cx.names.local(*local)))
                        .collect::<Vec<_>>()
                        .join(" and ");
                    writeln!(out, "{}if {guard}:", pad(indent))?;
                    writeln!(
                        out,
                        "{}_lir_return[{key:?}] = {}",
                        pad(indent + 1),
                        expr(value, cx.names)?
                    )?;
                }
            }
        }
    }
    writeln!(out, "{}return (\"return\", _lir_return)", pad(indent))?;
    Ok(())
}

fn params_tuple(params: String) -> String {
    if params.is_empty() {
        "()".to_owned()
    } else {
        format!("({params},)")
    }
}

fn mark_stmt_defs(stmt: &Stmt, defined: &mut HashSet<LocalId>) {
    match stmt {
        Stmt::Assign { dst, .. } => {
            defined.insert(*dst);
        }
        Stmt::Unsupported { dsts, .. } => {
            defined.extend(dsts.iter().copied());
        }
        Stmt::Capture { .. } | Stmt::Expr(_) => {}
    }
}

fn expr_local_ids(expr: &Expr) -> Vec<LocalId> {
    let mut locals = Vec::new();
    collect_expr_local_ids(expr, &mut locals);
    locals.sort();
    locals.dedup();
    locals
}

fn collect_expr_local_ids(expr: &Expr, locals: &mut Vec<LocalId>) {
    match expr {
        Expr::Local(local) => locals.push(*local),
        Expr::Const(_) => {}
        Expr::Unary { arg, .. } => collect_expr_local_ids(arg, locals),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_local_ids(lhs, locals);
            collect_expr_local_ids(rhs, locals);
        }
        Expr::Call { args, .. } | Expr::Unsupported { args, .. } => {
            for arg in args {
                collect_expr_local_ids(arg, locals);
            }
        }
    }
}

struct EmitCx<'a> {
    names: &'a NameTable,
    helper_params: HashMap<Label, Vec<LocalId>>,
    optional_helper_live_ins: &'a HashSet<(Label, LocalId)>,
    helper_prefix: &'a str,
    captures_outputs: bool,
}

fn helper_params_signature(
    names: &NameTable,
    params: &[LocalId],
    captures_outputs: bool,
) -> String {
    let mut rendered = if captures_outputs { vec!["_lir_outputs".to_owned()] } else { Vec::new() };
    rendered.extend(params.iter().map(|param| names.local(*param)));
    rendered.join(", ")
}

fn entry_args_tuple(params: String, captures_outputs: bool) -> String {
    let mut args = if captures_outputs { vec!["_lir_outputs".to_owned()] } else { Vec::new() };
    args.extend(params.split(", ").filter(|param| !param.is_empty()).map(str::to_owned));
    if args.is_empty() {
        "()".to_owned()
    } else {
        format!("({},)", args.join(", "))
    }
}

fn unary_expr(op: UnaryOp, arg: String) -> String {
    match op {
        UnaryOp::Not => format!("not ({arg})"),
        UnaryOp::Neg => format!("-({arg})"),
        UnaryOp::Cast(crate::lir::LirType::Bool) => format!("bool({arg})"),
        UnaryOp::Cast(crate::lir::LirType::Int) => format!("int({arg})"),
        UnaryOp::Cast(crate::lir::LirType::Real) => format!("float({arg})"),
        UnaryOp::Cast(_) => arg,
        UnaryOp::Math1(MathUnary::Clog2) => format!("math.ceil(math.log2({arg}))"),
        UnaryOp::Math1(op) => format!("{}({arg})", math_unary(op)),
    }
}

fn binary_expr(op: BinaryOp, lhs: String, rhs: String) -> String {
    match op {
        BinaryOp::Add => format!("({lhs}) + ({rhs})"),
        BinaryOp::Sub => format!("({lhs}) - ({rhs})"),
        BinaryOp::Mul => format!("({lhs}) * ({rhs})"),
        BinaryOp::Div => format!("({lhs}) / ({rhs})"),
        BinaryOp::Rem => format!("({lhs}) % ({rhs})"),
        BinaryOp::Shl => format!("({lhs}) << ({rhs})"),
        BinaryOp::Shr => format!("({lhs}) >> ({rhs})"),
        BinaryOp::BitAnd => format!("({lhs}) & ({rhs})"),
        BinaryOp::BitOr => format!("({lhs}) | ({rhs})"),
        BinaryOp::BitXor => format!("({lhs}) ^ ({rhs})"),
        BinaryOp::Eq => format!("({lhs}) == ({rhs})"),
        BinaryOp::Ne => format!("({lhs}) != ({rhs})"),
        BinaryOp::Lt => format!("({lhs}) < ({rhs})"),
        BinaryOp::Le => format!("({lhs}) <= ({rhs})"),
        BinaryOp::Gt => format!("({lhs}) > ({rhs})"),
        BinaryOp::Ge => format!("({lhs}) >= ({rhs})"),
        BinaryOp::Math2(op) => format!("{}({lhs}, {rhs})", math_binary(op)),
    }
}

fn math_unary(op: MathUnary) -> &'static str {
    match op {
        MathUnary::Sqrt => "math.sqrt",
        MathUnary::Exp => "math.exp",
        MathUnary::Ln => "math.log",
        MathUnary::Log10 => "math.log10",
        MathUnary::Clog2 => unreachable!("clog2 needs nested Python calls"),
        MathUnary::Floor => "math.floor",
        MathUnary::Ceil => "math.ceil",
        MathUnary::Sin => "math.sin",
        MathUnary::Cos => "math.cos",
        MathUnary::Tan => "math.tan",
        MathUnary::Asin => "math.asin",
        MathUnary::Acos => "math.acos",
        MathUnary::Atan => "math.atan",
        MathUnary::Sinh => "math.sinh",
        MathUnary::Cosh => "math.cosh",
        MathUnary::Tanh => "math.tanh",
        MathUnary::Asinh => "math.asinh",
        MathUnary::Acosh => "math.acosh",
        MathUnary::Atanh => "math.atanh",
    }
}

fn math_binary(op: MathBinary) -> &'static str {
    match op {
        MathBinary::Hypot => "math.hypot",
        MathBinary::Atan2 => "math.atan2",
        MathBinary::Pow => "math.pow",
    }
}

struct NameTable {
    locals: HashMap<LocalId, String>,
}

impl NameTable {
    fn new(function: &Function) -> Self {
        let mut used = HashMap::<String, usize>::new();
        let mut locals = HashMap::new();
        for local in &function.locals {
            let base = sanitize_ident(&local.name_hint);
            let count = used.entry(base.clone()).or_insert(0);
            let name = if *count == 0 { base.clone() } else { format!("{base}_{count}") };
            *count += 1;
            locals.insert(local.id, name);
        }
        Self { locals }
    }

    fn params(&self, function: &Function) -> String {
        function.params.iter().map(|param| self.local(*param)).collect::<Vec<_>>().join(", ")
    }

    fn local(&self, local: LocalId) -> String {
        self.locals.get(&local).cloned().unwrap_or_else(|| format!("l{}", local.0))
    }
}

fn helper_name(function_name: &str, label: Label) -> String {
    format!("_{function_name}_bb_{}", label.0)
}

fn entry_helper_name(function_name: &str) -> String {
    format!("_{function_name}_entry")
}

fn sanitize_ident(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("lifted");
    }
    if out.as_bytes()[0].is_ascii_digit() {
        out.insert(0, '_');
    }
    out
}

fn pad(indent: usize) -> String {
    "    ".repeat(indent)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::emit_function;
    use crate::lir::{
        Block, ConstValue, Expr, Function, Label, LirType, Local, LocalId, Stmt, Terminator,
    };

    #[test]
    fn guarded_capture_clears_stale_output_when_input_is_undefined() {
        let cond = LocalId(0);
        let stale = LocalId(1);
        let maybe = LocalId(2);
        let function = Function {
            name: "stale_capture".to_owned(),
            params: vec![cond],
            locals: vec![
                Local { id: cond, name_hint: "cond".to_owned(), ty: LirType::Bool },
                Local { id: stale, name_hint: "stale".to_owned(), ty: LirType::Int },
                Local { id: maybe, name_hint: "maybe".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: vec![
                Block {
                    label: Label(0),
                    stmts: vec![
                        Stmt::Assign { dst: stale, value: Expr::Const(ConstValue::Int(7)) },
                        Stmt::Capture { key: "slot".to_owned(), value: Expr::Local(stale) },
                    ],
                    term: Terminator::Branch {
                        cond: Expr::Local(cond),
                        then_label: Label(1),
                        else_label: Label(2),
                    },
                },
                Block {
                    label: Label(1),
                    stmts: vec![Stmt::Capture {
                        key: "slot".to_owned(),
                        value: Expr::Local(maybe),
                    }],
                    term: Terminator::Return(Vec::new()),
                },
                Block {
                    label: Label(2),
                    stmts: vec![Stmt::Assign {
                        dst: maybe,
                        value: Expr::Const(ConstValue::Int(9)),
                    }],
                    term: Terminator::Goto(Label(1)),
                },
            ],
            returns: Vec::new(),
        };

        let emitted = emit_function(&function).unwrap();
        assert!(emitted.contains(r#"_lir_outputs.pop("slot", None)"#), "{emitted}");

        let script = format!(
            "{emitted}\n\
             assert stale_capture(True) == {{}}, stale_capture(True)\n\
             assert stale_capture(False) == {{'slot': 9}}, stale_capture(False)\n"
        );
        let output =
            Command::new("python3").arg("-c").arg(script).output().expect("failed to run python3");
        assert!(
            output.status.success(),
            "python failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
