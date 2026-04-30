use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::Result;

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
    let mut out = String::new();
    let cx = EmitCx { names: &names, helper_params, helper_prefix: &function_name };

    for helper in &structured.helpers {
        writeln!(
            out,
            "def {}({}):",
            helper_name(&function_name, helper.label),
            helper.params.iter().map(|param| names.local(*param)).collect::<Vec<_>>().join(", ")
        )?;
        let mut defined = helper.params.iter().copied().collect::<HashSet<_>>();
        emit_structured_body(&mut out, &helper.body, &cx, 1, &mut defined)?;
        writeln!(out)?;
    }

    writeln!(out, "def {}({}):", function_name, names.params(function))?;
    let mut defined = function.params.iter().copied().collect::<HashSet<_>>();
    emit_structured_body(&mut out, &structured.body, &cx, 1, &mut defined)?;

    Ok(out)
}

fn emit_stmt(out: &mut String, stmt: &Stmt, names: &NameTable, indent: usize) -> Result<()> {
    match stmt {
        Stmt::Assign { dst, value } => {
            writeln!(out, "{}{} = {}", pad(indent), names.local(*dst), expr(value, names))?;
        }
        Stmt::Expr(value) => {
            writeln!(out, "{}{}", pad(indent), expr(value, names))?;
        }
        Stmt::Unsupported { dsts, text } if dsts.is_empty() => {
            writeln!(out, "{}mir_unlifted({text:?})", pad(indent))?;
        }
        Stmt::Unsupported { dsts, text } if dsts.len() == 1 => {
            writeln!(out, "{}{} = mir_unlifted({text:?})", pad(indent), names.local(dsts[0]))?;
        }
        Stmt::Unsupported { dsts, text } => {
            let lhs = dsts.iter().map(|dst| names.local(*dst)).collect::<Vec<_>>().join(", ");
            writeln!(out, "{}{} = mir_unlifted({text:?})", pad(indent), lhs)?;
        }
    }
    Ok(())
}

fn expr(value: &Expr, names: &NameTable) -> String {
    match value {
        Expr::Local(local) => names.local(*local),
        Expr::Const(ConstValue::Bool(true)) => "True".to_owned(),
        Expr::Const(ConstValue::Bool(false)) => "False".to_owned(),
        Expr::Const(ConstValue::Int(value)) => value.to_string(),
        Expr::Const(ConstValue::Real(value)) => value.to_string(),
        Expr::Const(ConstValue::Str(value)) => format!("{value:?}"),
        Expr::Const(ConstValue::None) => "None".to_owned(),
        Expr::Unary { op, arg } => unary_expr(*op, expr(arg, names)),
        Expr::Binary { op, lhs, rhs } => binary_expr(*op, expr(lhs, names), expr(rhs, names)),
        Expr::Call { target, args } => {
            let args = args.iter().map(|arg| expr(arg, names)).collect::<Vec<_>>().join(", ");
            if args.is_empty() {
                format!("mir_call({target:?})")
            } else {
                format!("mir_call({target:?}, {args})")
            }
        }
        Expr::Unsupported { text, .. } => format!("mir_unlifted({text:?})"),
    }
}

fn emit_structured_body(
    out: &mut String,
    body: &[StructuredStmt],
    cx: &EmitCx<'_>,
    indent: usize,
    defined: &mut HashSet<LocalId>,
) -> Result<()> {
    if body.is_empty() {
        writeln!(out, "{}pass", pad(indent))?;
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
            writeln!(out, "{}if {}:", pad(indent), expr(cond, cx.names))?;
            let mut then_defined = defined.clone();
            emit_structured_body(out, then_body, cx, indent + 1, &mut then_defined)?;
            writeln!(out, "{}else:", pad(indent))?;
            let mut else_defined = defined.clone();
            emit_structured_body(out, else_body, cx, indent + 1, &mut else_defined)?;
            then_defined.retain(|local| else_defined.contains(local));
            *defined = then_defined;
        }
        StructuredStmt::CallHelper(label) => {
            let args =
                cx.helper_params
                    .get(label)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|arg| {
                        if defined.contains(&arg) {
                            cx.names.local(arg)
                        } else {
                            "None".to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
            writeln!(
                out,
                "{}return {}({args})",
                pad(indent),
                helper_name(cx.helper_prefix, *label)
            )?;
        }
        StructuredStmt::Return(values) => {
            let entries = values
                .iter()
                .map(|value| match value {
                    crate::lir::ReturnValue::Named { key, value } => {
                        format!("{key:?}: {}", expr(value, cx.names))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "{}return {{{entries}}}", pad(indent))?;
        }
        StructuredStmt::Raise(message) => {
            writeln!(out, "{}raise RuntimeError({message:?})", pad(indent))?;
        }
    }
    Ok(())
}

fn mark_stmt_defs(stmt: &Stmt, defined: &mut HashSet<LocalId>) {
    match stmt {
        Stmt::Assign { dst, .. } => {
            defined.insert(*dst);
        }
        Stmt::Unsupported { dsts, .. } => {
            defined.extend(dsts.iter().copied());
        }
        Stmt::Expr(_) => {}
    }
}

struct EmitCx<'a> {
    names: &'a NameTable,
    helper_params: HashMap<Label, Vec<LocalId>>,
    helper_prefix: &'a str,
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
