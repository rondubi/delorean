use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::{bail, Result};

use crate::lir::{
    BinaryOp, CallEffect, ConstValue, Expr, Function, Label, LocalId, MathBinary, MathUnary, Stmt,
    UnaryOp,
};
use crate::lir_structure::{self, StructuredStmt};

pub(crate) fn emit_function(function: &Function) -> Result<String> {
    let names = NameTable::new(function);
    let function_name = sanitize_ident(&function.name);
    let start = Instant::now();
    let structured = lir_structure::structure(function)?;
    validate_direct_helper_graph(&structured)?;
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
    let captures_effects = structured_captures_effects(&structured);
    let output_layout = OutputLayout::new(function, captures_effects);
    let mut out = String::new();
    let mut cx = EmitCx {
        names: &names,
        helper_params,
        optional_helper_live_ins: &structured.facts.optional_helper_live_ins,
        helper_prefix: &function_name,
        captures_outputs,
        captures_effects,
        output_layout,
        current_helper: None,
    };

    writeln!(out, "_LIR_UNDEF = object()")?;
    writeln!(out)?;

    for helper in &structured.helpers {
        cx.current_helper = Some(helper.label);
        writeln!(
            out,
            "def {}({}):",
            helper_name(&function_name, helper.label),
            helper_params_signature(
                &names,
                helper.params.as_slice(),
                captures_outputs,
                captures_effects
            )
        )?;
        let mut assigned = helper.params.iter().copied().collect::<HashSet<_>>();
        let mut defined = helper
            .params
            .iter()
            .copied()
            .filter(|param| !cx.optional_helper_live_ins.contains(&(helper.label, *param)))
            .collect::<HashSet<_>>();
        if body_calls_helper(&helper.body, helper.label) {
            writeln!(out, "    while True:")?;
            emit_structured_body(
                &mut out,
                &helper.body,
                &cx,
                2,
                &mut assigned,
                &mut defined,
                EmptyBody::Pass,
            )?;
        } else {
            emit_structured_body(
                &mut out,
                &helper.body,
                &cx,
                1,
                &mut assigned,
                &mut defined,
                EmptyBody::Return,
            )?;
        }
        writeln!(out)?;
    }

    let entry_name = entry_helper_name(&function_name);
    cx.current_helper = None;
    writeln!(
        out,
        "def {}({}):",
        entry_name,
        helper_params_signature(
            &names,
            function.params.as_slice(),
            captures_outputs,
            captures_effects
        )
    )?;
    let mut assigned = function.params.iter().copied().collect::<HashSet<_>>();
    let mut defined = assigned.clone();
    emit_structured_body(
        &mut out,
        &structured.body,
        &cx,
        1,
        &mut assigned,
        &mut defined,
        EmptyBody::Return,
    )?;
    writeln!(out)?;

    writeln!(out, "def {}({}):", function_name, names.params(function))?;
    if captures_outputs {
        writeln!(out, "    _lir_outputs = [None] * {}", cx.output_layout.value_len)?;
    }
    if captures_effects {
        writeln!(out, "    _lir_invalid_params = []")?;
    }
    writeln!(
        out,
        "    return {}({})",
        entry_name,
        entry_args_list(names.params(function), captures_outputs, captures_effects)
    )?;

    Ok(out)
}

fn emit_stmt(
    out: &mut String,
    stmt: &Stmt,
    cx: &EmitCx<'_>,
    indent: usize,
    defined: &HashSet<LocalId>,
) -> Result<()> {
    match stmt {
        Stmt::Assign { dst, value } => {
            writeln!(out, "{}{} = {}", pad(indent), cx.names.local(*dst), expr(value, cx.names)?)?;
        }
        Stmt::Capture { key, value } => {
            emit_capture(out, key, value, cx, indent, defined)?;
        }
        Stmt::CallEffect(effect) => emit_call_effect(out, effect, cx.names, indent)?,
        Stmt::Expr(value) => {
            writeln!(out, "{}{}", pad(indent), expr(value, cx.names)?)?;
        }
        Stmt::Unsupported { text, .. } => {
            bail!("unsupported LIR statement: {text}");
        }
    }
    Ok(())
}

fn emit_call_effect(
    out: &mut String,
    effect: &CallEffect,
    _names: &NameTable,
    indent: usize,
) -> Result<()> {
    match effect {
        CallEffect::Diagnostic { .. } => {}
        CallEffect::SetInvalidParam { param } => {
            writeln!(out, "{}_lir_invalid_params.append({param:?})", pad(indent),)?;
        }
        CallEffect::CollapseHint { .. } => {}
    }
    Ok(())
}

fn emit_capture(
    out: &mut String,
    key: &str,
    value: &Expr,
    cx: &EmitCx<'_>,
    indent: usize,
    defined: &HashSet<LocalId>,
) -> Result<()> {
    let slot = cx.output_layout.slot(key)?;
    let undefined = expr_undefined_local_ids(value, defined);
    if undefined.is_empty() {
        writeln!(out, "{}_lir_outputs[{slot}] = {}", pad(indent), expr(value, cx.names)?)?;
        return Ok(());
    }

    let guard = undefined
        .iter()
        .map(|local| format!("{} is not _LIR_UNDEF", cx.names.local(*local)))
        .collect::<Vec<_>>()
        .join(" and ");
    writeln!(out, "{}if {guard}:", pad(indent))?;
    writeln!(out, "{}_lir_outputs[{slot}] = {}", pad(indent + 1), expr(value, cx.names)?)?;
    writeln!(out, "{}else:", pad(indent))?;
    writeln!(out, "{}_lir_outputs[{slot}] = None", pad(indent + 1))?;
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
        Expr::SimparamOpt { name, default } => {
            format!("_lir_simparam_opt({}, {})", expr(name, names)?, expr(default, names)?)
        }
        Expr::Call { target, .. } => {
            bail!("unsupported LIR call target during Python emission: {target}");
        }
        Expr::Unsupported { text, .. } => bail!("unsupported LIR expression: {text}"),
    })
}

#[derive(Clone, Copy)]
enum EmptyBody {
    Pass,
    Return,
}

fn emit_structured_body(
    out: &mut String,
    body: &[StructuredStmt],
    cx: &EmitCx<'_>,
    indent: usize,
    assigned: &mut HashSet<LocalId>,
    defined: &mut HashSet<LocalId>,
    empty: EmptyBody,
) -> Result<()> {
    if body.is_empty() {
        match empty {
            EmptyBody::Pass => writeln!(out, "{}pass", pad(indent))?,
            EmptyBody::Return => emit_return(out, &[], cx, indent, defined)?,
        }
        return Ok(());
    }

    for stmt in body {
        emit_structured_stmt(out, stmt, cx, indent, assigned, defined)?;
    }
    Ok(())
}

fn emit_structured_stmt(
    out: &mut String,
    stmt: &StructuredStmt,
    cx: &EmitCx<'_>,
    indent: usize,
    assigned: &mut HashSet<LocalId>,
    defined: &mut HashSet<LocalId>,
) -> Result<()> {
    match stmt {
        StructuredStmt::Stmt(stmt) => {
            emit_stmt(out, stmt, cx, indent, defined)?;
            mark_stmt_defs(stmt, assigned, defined);
        }
        StructuredStmt::If { cond, then_body, else_body } => {
            writeln!(out, "{}if {}:", pad(indent), expr(cond, cx.names)?)?;
            let mut then_assigned = assigned.clone();
            let mut then_defined = defined.clone();
            emit_structured_body(
                out,
                then_body,
                cx,
                indent + 1,
                &mut then_assigned,
                &mut then_defined,
                EmptyBody::Pass,
            )?;
            writeln!(out, "{}else:", pad(indent))?;
            let mut else_assigned = assigned.clone();
            let mut else_defined = defined.clone();
            emit_structured_body(
                out,
                else_body,
                cx,
                indent + 1,
                &mut else_assigned,
                &mut else_defined,
                EmptyBody::Pass,
            )?;
            then_assigned.retain(|local| else_assigned.contains(local));
            then_defined.retain(|local| else_defined.contains(local));
            *assigned = then_assigned;
            *defined = then_defined;
        }
        StructuredStmt::CallHelper(label) => {
            if Some(*label) == cx.current_helper {
                emit_self_loop_continue(out, *label, cx, indent, assigned)?;
            } else {
                let args = helper_args_list(*label, cx, assigned)?;
                writeln!(
                    out,
                    "{}return {}({args})",
                    pad(indent),
                    helper_name(cx.helper_prefix, *label)
                )?;
            }
        }
        StructuredStmt::Return(values) => {
            emit_return(out, values, cx, indent, defined)?;
        }
        StructuredStmt::Raise(message) => {
            writeln!(out, "{}raise RuntimeError({message:?})", pad(indent))?;
        }
    }
    Ok(())
}

fn emit_self_loop_continue(
    out: &mut String,
    label: Label,
    cx: &EmitCx<'_>,
    indent: usize,
    assigned: &HashSet<LocalId>,
) -> Result<()> {
    let params = cx.helper_params.get(&label).cloned().unwrap_or_default();
    let args = helper_param_arg_exprs(label, cx, assigned)?;
    if !params.is_empty() {
        let targets = params.iter().map(|param| cx.names.local(*param)).collect::<Vec<_>>();
        writeln!(out, "{}{} = {}", pad(indent), tuple_items(&targets), tuple_items(&args))?;
    }
    writeln!(out, "{}continue", pad(indent))?;
    Ok(())
}

fn helper_args_list(label: Label, cx: &EmitCx<'_>, assigned: &HashSet<LocalId>) -> Result<String> {
    let mut args = if cx.captures_outputs { vec!["_lir_outputs".to_owned()] } else { Vec::new() };
    if cx.captures_effects {
        args.push("_lir_invalid_params".to_owned());
    }
    args.extend(helper_param_arg_exprs(label, cx, assigned)?);
    Ok(args.join(", "))
}

fn helper_param_arg_exprs(
    label: Label,
    cx: &EmitCx<'_>,
    assigned: &HashSet<LocalId>,
) -> Result<Vec<String>> {
    cx.helper_params
        .get(&label)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|arg| -> Result<String> {
            if !assigned.contains(&arg) && !cx.optional_helper_live_ins.contains(&(label, arg)) {
                bail!(
                    "structured LIR helper {} requires {} before it is defined",
                    helper_name(cx.helper_prefix, label),
                    cx.names.local(arg)
                );
            }
            if assigned.contains(&arg) {
                Ok(cx.names.local(arg))
            } else {
                Ok("_LIR_UNDEF".to_owned())
            }
        })
        .collect()
}

fn tuple_items(items: &[String]) -> String {
    match items {
        [] => "()".to_owned(),
        [item] => format!("{item},"),
        _ => items.join(", "),
    }
}

fn emit_return(
    out: &mut String,
    values: &[crate::lir::ReturnValue],
    cx: &EmitCx<'_>,
    indent: usize,
    defined: &HashSet<LocalId>,
) -> Result<()> {
    if return_values_are_definitely_defined(values, defined) {
        if cx.output_layout.uses_slots() {
            if cx.captures_outputs {
                writeln!(out, "{}_lir_return = list(_lir_outputs)", pad(indent))?;
            } else {
                writeln!(
                    out,
                    "{}_lir_return = [None] * {}",
                    pad(indent),
                    cx.output_layout.value_len
                )?;
            }
            for value in values {
                match value {
                    crate::lir::ReturnValue::Named { key, value } => {
                        let slot = cx.output_layout.slot(key)?;
                        writeln!(
                            out,
                            "{}_lir_return[{slot}] = {}",
                            pad(indent),
                            expr(value, cx.names)?
                        )?;
                    }
                }
            }
            if cx.captures_effects {
                writeln!(out, "{}_lir_return.append([0, _lir_invalid_params])", pad(indent))?;
            }
            writeln!(out, "{}return _lir_return", pad(indent))?;
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
        writeln!(out, "{}return {{{entries}}}", pad(indent))?;
        return Ok(());
    }

    if cx.captures_outputs {
        writeln!(out, "{}_lir_return = list(_lir_outputs)", pad(indent))?;
    } else {
        writeln!(out, "{}_lir_return = [None] * {}", pad(indent), cx.output_layout.value_len)?;
    }
    if cx.captures_effects {
        writeln!(out, "{}_lir_return.append([0, _lir_invalid_params])", pad(indent))?;
    }
    for value in values {
        match value {
            crate::lir::ReturnValue::Named { key, value } => {
                let slot = cx.output_layout.slot(key)?;
                let undefined = expr_undefined_local_ids(value, defined);
                if undefined.is_empty() {
                    writeln!(
                        out,
                        "{}_lir_return[{slot}] = {}",
                        pad(indent),
                        expr(value, cx.names)?
                    )?;
                } else {
                    let guard = undefined
                        .iter()
                        .map(|local| format!("{} is not _LIR_UNDEF", cx.names.local(*local)))
                        .collect::<Vec<_>>()
                        .join(" and ");
                    writeln!(out, "{}if {guard}:", pad(indent))?;
                    writeln!(
                        out,
                        "{}_lir_return[{slot}] = {}",
                        pad(indent + 1),
                        expr(value, cx.names)?
                    )?;
                }
            }
        }
    }
    writeln!(out, "{}return _lir_return", pad(indent))?;
    Ok(())
}

fn mark_stmt_defs(stmt: &Stmt, assigned: &mut HashSet<LocalId>, defined: &mut HashSet<LocalId>) {
    match stmt {
        Stmt::Assign { dst, value } => {
            assigned.insert(*dst);
            if expr_is_definitely_defined(value, defined) {
                defined.insert(*dst);
            } else {
                defined.remove(dst);
            }
        }
        Stmt::Unsupported { dsts, .. } => {
            assigned.extend(dsts.iter().copied());
            defined.extend(dsts.iter().copied());
        }
        Stmt::Capture { .. } | Stmt::CallEffect(_) | Stmt::Expr(_) => {}
    }
}

fn return_values_are_definitely_defined(
    values: &[crate::lir::ReturnValue],
    defined: &HashSet<LocalId>,
) -> bool {
    values.iter().all(|value| match value {
        crate::lir::ReturnValue::Named { value, .. } => expr_is_definitely_defined(value, defined),
    })
}

fn expr_is_definitely_defined(expr: &Expr, defined: &HashSet<LocalId>) -> bool {
    expr_undefined_local_ids(expr, defined).is_empty()
}

fn expr_undefined_local_ids(expr: &Expr, defined: &HashSet<LocalId>) -> Vec<LocalId> {
    expr_local_ids(expr).into_iter().filter(|local| !defined.contains(local)).collect()
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
        Expr::SimparamOpt { name, default } => {
            collect_expr_local_ids(name, locals);
            collect_expr_local_ids(default, locals);
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
    captures_effects: bool,
    output_layout: OutputLayout,
    current_helper: Option<Label>,
}

#[derive(Clone, Debug)]
struct OutputLayout {
    slots: HashMap<String, usize>,
    value_len: usize,
    state_slot: Option<usize>,
}

impl OutputLayout {
    fn new(function: &Function, captures_effects: bool) -> Self {
        let mut keys = BTreeMap::<String, ()>::new();
        for block in &function.blocks {
            for stmt in &block.stmts {
                if let Stmt::Capture { key, .. } = stmt {
                    keys.insert(key.clone(), ());
                }
            }
            if let crate::lir::Terminator::Return(values) = &block.term {
                for value in values {
                    match value {
                        crate::lir::ReturnValue::Named { key, .. } => {
                            keys.insert(key.clone(), ());
                        }
                    }
                }
            }
        }

        let slots =
            keys.into_keys().enumerate().map(|(slot, key)| (key, slot)).collect::<HashMap<_, _>>();
        let value_len = slots.len();
        let state_slot = captures_effects.then_some(value_len);
        Self { slots, value_len, state_slot }
    }

    fn uses_slots(&self) -> bool {
        self.value_len != 0 || self.state_slot.is_some()
    }

    fn slot(&self, key: &str) -> Result<usize> {
        self.slots
            .get(key)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("missing LIR output slot for {key:?}"))
    }
}

fn validate_direct_helper_graph(structured: &lir_structure::StructuredFunction) -> Result<()> {
    let graph = structured
        .helpers
        .iter()
        .map(|helper| {
            if body_calls_helper(&helper.body, helper.label) {
                validate_self_loop_body(helper.label, &helper.body)?;
            }
            let mut calls = Vec::new();
            collect_structured_helper_calls(&helper.body, &mut calls);
            calls.retain(|label| *label != helper.label);
            calls.sort();
            calls.dedup();
            Ok((helper.label, calls))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let mut visited = HashSet::new();
    let mut stack = Vec::new();
    let mut labels = graph.keys().copied().collect::<Vec<_>>();
    labels.sort();

    for label in labels {
        if let Some(cycle) = direct_helper_cycle(label, &graph, &mut visited, &mut stack) {
            let path =
                cycle.into_iter().map(|label| label.to_string()).collect::<Vec<_>>().join(" -> ");
            bail!("structured LIR helper cycle cannot be emitted as direct Python calls: {path}");
        }
    }

    Ok(())
}

fn validate_self_loop_body(label: Label, body: &[StructuredStmt]) -> Result<()> {
    let mut saw_self_call = false;
    validate_self_loop_body_inner(label, body, &mut saw_self_call)?;
    if !saw_self_call {
        bail!("structured LIR helper {label} was selected as a loop without a self edge");
    }
    Ok(())
}

fn validate_self_loop_body_inner(
    label: Label,
    body: &[StructuredStmt],
    saw_self_call: &mut bool,
) -> Result<()> {
    for stmt in body {
        validate_self_loop_stmt(label, stmt, saw_self_call)?;
    }
    Ok(())
}

fn validate_self_loop_stmt(
    label: Label,
    stmt: &StructuredStmt,
    saw_self_call: &mut bool,
) -> Result<()> {
    match stmt {
        StructuredStmt::CallHelper(target) if *target == label => {
            *saw_self_call = true;
        }
        StructuredStmt::If { then_body, else_body, .. } => {
            validate_self_loop_body_inner(label, then_body, saw_self_call)?;
            validate_self_loop_body_inner(label, else_body, saw_self_call)?;
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => {}
    }
    Ok(())
}

fn direct_helper_cycle(
    label: Label,
    graph: &HashMap<Label, Vec<Label>>,
    visited: &mut HashSet<Label>,
    stack: &mut Vec<Label>,
) -> Option<Vec<Label>> {
    if let Some(pos) = stack.iter().position(|active| *active == label) {
        let mut cycle = stack[pos..].to_vec();
        cycle.push(label);
        return Some(cycle);
    }
    if visited.contains(&label) {
        return None;
    }

    stack.push(label);
    if let Some(targets) = graph.get(&label) {
        for target in targets {
            if let Some(cycle) = direct_helper_cycle(*target, graph, visited, stack) {
                return Some(cycle);
            }
        }
    }
    stack.pop();
    visited.insert(label);
    None
}

fn body_calls_helper(body: &[StructuredStmt], target: Label) -> bool {
    body.iter().any(|stmt| stmt_calls_helper(stmt, target))
}

fn stmt_calls_helper(stmt: &StructuredStmt, target: Label) -> bool {
    match stmt {
        StructuredStmt::CallHelper(label) => *label == target,
        StructuredStmt::If { then_body, else_body, .. } => {
            body_calls_helper(then_body, target) || body_calls_helper(else_body, target)
        }
        StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => false,
    }
}

fn collect_structured_helper_calls(body: &[StructuredStmt], calls: &mut Vec<Label>) {
    for stmt in body {
        match stmt {
            StructuredStmt::CallHelper(label) => calls.push(*label),
            StructuredStmt::If { then_body, else_body, .. } => {
                collect_structured_helper_calls(then_body, calls);
                collect_structured_helper_calls(else_body, calls);
            }
            StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => {}
        }
    }
}

fn helper_params_signature(
    names: &NameTable,
    params: &[LocalId],
    captures_outputs: bool,
    captures_effects: bool,
) -> String {
    let mut rendered = if captures_outputs { vec!["_lir_outputs".to_owned()] } else { Vec::new() };
    if captures_effects {
        rendered.push("_lir_invalid_params".to_owned());
    }
    rendered.extend(params.iter().map(|param| names.local(*param)));
    rendered.join(", ")
}

fn entry_args_list(params: String, captures_outputs: bool, captures_effects: bool) -> String {
    let mut args = if captures_outputs { vec!["_lir_outputs".to_owned()] } else { Vec::new() };
    if captures_effects {
        args.push("_lir_invalid_params".to_owned());
    }
    args.extend(params.split(", ").filter(|param| !param.is_empty()).map(str::to_owned));
    args.join(", ")
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

fn structured_captures_effects(structured: &lir_structure::StructuredFunction) -> bool {
    body_captures_effects(&structured.body)
        || structured.helpers.iter().any(|helper| body_captures_effects(&helper.body))
}

fn body_captures_effects(body: &[StructuredStmt]) -> bool {
    body.iter().any(stmt_captures_effects)
}

fn stmt_captures_effects(stmt: &StructuredStmt) -> bool {
    match stmt {
        StructuredStmt::Stmt(Stmt::CallEffect(CallEffect::SetInvalidParam { .. })) => true,
        StructuredStmt::If { then_body, else_body, .. } => {
            body_captures_effects(then_body) || body_captures_effects(else_body)
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::process::Command;

    use super::emit_function;
    use crate::lir::{
        Block, CallEffect, ConstValue, Expr, Function, Label, LirType, Local, LocalId, ReturnValue,
        Stmt, Terminator,
    };

    fn assert_no_trampoline_protocol(emitted: &str) {
        for needle in
            ["(\"call\",", "(\"return\",", "_lir_target", "_lir_result[0]", "_lir_result", "_pc"]
        {
            assert!(!emitted.contains(needle), "found {needle:?} in:\n{emitted}");
        }
    }

    #[test]
    fn emits_explicit_call_semantics_without_mir_call_runtime() {
        let value = LocalId(0);
        let function = Function {
            name: "call_effects".to_owned(),
            params: Vec::new(),
            locals: vec![Local { id: value, name_hint: "val".to_owned(), ty: LirType::Real }],
            entry: Label(0),
            blocks: vec![Block {
                label: Label(0),
                stmts: vec![
                    Stmt::Assign {
                        dst: value,
                        value: Expr::SimparamOpt {
                            name: Box::new(Expr::Const(ConstValue::Str("gmin".to_owned()))),
                            default: Box::new(Expr::Const(ConstValue::Real(1e-12))),
                        },
                    },
                    Stmt::CallEffect(CallEffect::Diagnostic {
                        target: "Display)".to_owned(),
                        args: vec![Expr::Local(value)],
                    }),
                    Stmt::CallEffect(CallEffect::SetInvalidParam { param: "p7".to_owned() }),
                    Stmt::CallEffect(CallEffect::CollapseHint {
                        hi: "n2".to_owned(),
                        lo: Some("n1".to_owned()),
                    }),
                ],
                term: Terminator::Return(vec![ReturnValue::Named {
                    key: "out".to_owned(),
                    value: Expr::Local(value),
                }]),
            }],
            returns: Vec::new(),
        };

        let emitted = emit_function(&function).unwrap();
        assert_no_trampoline_protocol(&emitted);
        assert!(!emitted.contains(" = mir_call("), "{emitted}");
        assert!(!emitted.contains("MIR_IGNORED_CALLS"), "{emitted}");
        assert!(
            emitted.contains(r#"val = _lir_simparam_opt("gmin", 0.000000000001)"#),
            "{emitted}"
        );
        assert!(!emitted.contains(r#"_lir_state["diagnostics"].append"#), "{emitted}");
        assert!(!emitted.contains("diagnostics"), "{emitted}");
        assert!(emitted.contains(r#"_lir_invalid_params.append("p7")"#), "{emitted}");
        assert!(!emitted.contains("_lir_state"), "{emitted}");
        assert!(!emitted.contains("collapse_hints"), "{emitted}");
        assert!(!emitted.contains("_lir_effects.append"), "{emitted}");
        assert!(!emitted.contains(" ignored:"), "{emitted}");
        assert!(!emitted.contains("pass"), "{emitted}");

        let script = format!(
            "{emitted}\n\
             def _lir_simparam_opt(name, default): return {{'gmin': 2e-12}}.get(name, default)\n\
             result = call_effects()\n\
             assert result[0] == 2e-12, result\n\
             assert result[1][0] == 0, result\n\
             assert result[1][1] == ['p7'], result\n\
             assert len(result[1]) == 2, result\n"
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

    #[test]
    fn drops_values_used_only_by_non_semantic_effects() {
        let input = LocalId(0);
        let computed = LocalId(1);
        let function = Function {
            name: "effect_live_value".to_owned(),
            params: vec![input],
            locals: vec![
                Local { id: input, name_hint: "input".to_owned(), ty: LirType::Int },
                Local { id: computed, name_hint: "computed".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: vec![Block {
                label: Label(0),
                stmts: vec![
                    Stmt::Assign {
                        dst: computed,
                        value: Expr::Binary {
                            op: crate::lir::BinaryOp::Add,
                            lhs: Box::new(Expr::Local(input)),
                            rhs: Box::new(Expr::Const(ConstValue::Int(1))),
                        },
                    },
                    Stmt::CallEffect(CallEffect::Diagnostic {
                        target: "Display".to_owned(),
                        args: vec![Expr::Local(computed)],
                    }),
                    Stmt::CallEffect(CallEffect::CollapseHint {
                        hi: "n2".to_owned(),
                        lo: Some("n1".to_owned()),
                    }),
                ],
                term: Terminator::Return(Vec::new()),
            }],
            returns: Vec::new(),
        };

        let emitted = emit_function(&function).unwrap();
        assert!(!emitted.contains("computed = (input) + (1)"), "{emitted}");
        assert!(!emitted.contains(r#"_lir_state["diagnostics"].append"#), "{emitted}");
        assert!(!emitted.contains("_lir_state"), "{emitted}");
        assert!(!emitted.contains("collapse_hints"), "{emitted}");

        let script = format!(
            "{emitted}\n\
             result = effect_live_value(4)\n\
             assert result == {{}}, result\n"
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

    #[test]
    fn rejects_opaque_lir_call_during_python_emission() {
        let function = Function {
            name: "bad_call".to_owned(),
            params: Vec::new(),
            locals: Vec::new(),
            entry: Label(0),
            blocks: vec![Block {
                label: Label(0),
                stmts: vec![Stmt::Expr(Expr::Call {
                    target: "unknown_runtime".to_owned(),
                    args: Vec::new(),
                })],
                term: Terminator::Return(Vec::new()),
            }],
            returns: Vec::new(),
        };

        let error = emit_function(&function).unwrap_err().to_string();
        assert!(error.contains("unsupported LIR call target during Python emission"), "{error}");
    }

    #[test]
    fn emits_pass_for_empty_noop_branch_body() {
        let cond = LocalId(0);
        let function = Function {
            name: "noop_branch".to_owned(),
            params: vec![cond],
            locals: vec![Local { id: cond, name_hint: "cond".to_owned(), ty: LirType::Bool }],
            entry: Label(0),
            blocks: vec![Block {
                label: Label(0),
                stmts: Vec::new(),
                term: Terminator::Return(Vec::new()),
            }],
            returns: Vec::new(),
        };
        let names = super::NameTable::new(&function);
        let optional_helper_live_ins = HashSet::new();
        let cx = super::EmitCx {
            names: &names,
            helper_params: HashMap::new(),
            optional_helper_live_ins: &optional_helper_live_ins,
            helper_prefix: "noop_branch",
            captures_outputs: false,
            captures_effects: false,
            output_layout: super::OutputLayout::new(&function, false),
            current_helper: None,
        };
        let body = vec![crate::lir_structure::StructuredStmt::If {
            cond: Expr::Local(cond),
            then_body: Vec::new(),
            else_body: vec![crate::lir_structure::StructuredStmt::Stmt(Stmt::Expr(Expr::Const(
                ConstValue::Int(1),
            )))],
        }];
        let mut assigned = HashSet::from([cond]);
        let mut defined = assigned.clone();
        let mut emitted = String::new();
        super::emit_structured_body(
            &mut emitted,
            &body,
            &cx,
            1,
            &mut assigned,
            &mut defined,
            super::EmptyBody::Return,
        )
        .unwrap();

        assert!(!emitted.contains("empty LIR body"), "{emitted}");
        assert!(emitted.contains("if cond:"), "{emitted}");
        assert!(emitted.contains("    pass"), "{emitted}");
    }

    #[test]
    fn emits_direct_helper_calls_and_final_returns() {
        let cond = LocalId(0);
        let value = LocalId(1);
        let function = Function {
            name: "diamond".to_owned(),
            params: vec![cond],
            locals: vec![
                Local { id: cond, name_hint: "cond".to_owned(), ty: LirType::Bool },
                Local { id: value, name_hint: "v".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: vec![
                Block {
                    label: Label(0),
                    stmts: Vec::new(),
                    term: Terminator::Branch {
                        cond: Expr::Local(cond),
                        then_label: Label(1),
                        else_label: Label(2),
                    },
                },
                Block {
                    label: Label(1),
                    stmts: vec![Stmt::Assign {
                        dst: value,
                        value: Expr::Const(ConstValue::Int(1)),
                    }],
                    term: Terminator::Goto(Label(3)),
                },
                Block {
                    label: Label(2),
                    stmts: vec![Stmt::Assign {
                        dst: value,
                        value: Expr::Const(ConstValue::Int(2)),
                    }],
                    term: Terminator::Goto(Label(3)),
                },
                Block {
                    label: Label(3),
                    stmts: Vec::new(),
                    term: Terminator::Return(vec![ReturnValue::Named {
                        key: "out".to_owned(),
                        value: Expr::Local(value),
                    }]),
                },
            ],
            returns: Vec::new(),
        };

        let emitted = emit_function(&function).unwrap();
        assert_no_trampoline_protocol(&emitted);
        assert!(emitted.contains(r#"_lir_return[0] = v"#), "{emitted}");

        let script = format!(
            "{emitted}\n\
             assert diamond(True) == [1], diamond(True)\n\
             assert diamond(False) == [2], diamond(False)\n"
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

    #[test]
    fn emits_self_tail_helper_cycle_as_while_loop() {
        let n = LocalId(0);
        let i = LocalId(1);
        let acc = LocalId(2);
        let function = Function {
            name: "sum_to_n".to_owned(),
            params: vec![n],
            locals: vec![
                Local { id: n, name_hint: "n".to_owned(), ty: LirType::Int },
                Local { id: i, name_hint: "i".to_owned(), ty: LirType::Int },
                Local { id: acc, name_hint: "acc".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: vec![
                Block {
                    label: Label(0),
                    stmts: vec![
                        Stmt::Assign { dst: i, value: Expr::Const(ConstValue::Int(0)) },
                        Stmt::Assign { dst: acc, value: Expr::Const(ConstValue::Int(0)) },
                    ],
                    term: Terminator::Goto(Label(1)),
                },
                Block {
                    label: Label(1),
                    stmts: Vec::new(),
                    term: Terminator::Branch {
                        cond: Expr::Binary {
                            op: crate::lir::BinaryOp::Lt,
                            lhs: Box::new(Expr::Local(i)),
                            rhs: Box::new(Expr::Local(n)),
                        },
                        then_label: Label(2),
                        else_label: Label(3),
                    },
                },
                Block {
                    label: Label(2),
                    stmts: vec![
                        Stmt::Assign {
                            dst: acc,
                            value: Expr::Binary {
                                op: crate::lir::BinaryOp::Add,
                                lhs: Box::new(Expr::Local(acc)),
                                rhs: Box::new(Expr::Local(i)),
                            },
                        },
                        Stmt::Assign {
                            dst: i,
                            value: Expr::Binary {
                                op: crate::lir::BinaryOp::Add,
                                lhs: Box::new(Expr::Local(i)),
                                rhs: Box::new(Expr::Const(ConstValue::Int(1))),
                            },
                        },
                    ],
                    term: Terminator::Goto(Label(1)),
                },
                Block {
                    label: Label(3),
                    stmts: Vec::new(),
                    term: Terminator::Return(vec![ReturnValue::Named {
                        key: "out".to_owned(),
                        value: Expr::Local(acc),
                    }]),
                },
            ],
            returns: Vec::new(),
        };

        let emitted = emit_function(&function).unwrap();
        assert_no_trampoline_protocol(&emitted);
        assert!(emitted.contains("while True:"), "{emitted}");
        assert!(emitted.contains("continue"), "{emitted}");

        let script = format!(
            "{emitted}\n\
             assert sum_to_n(0) == [0], sum_to_n(0)\n\
             assert sum_to_n(5) == [10], sum_to_n(5)\n"
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

    #[test]
    fn rejects_multi_helper_cycle_instead_of_recursive_python_calls() {
        let structured = crate::lir_structure::StructuredFunction {
            body: vec![crate::lir_structure::StructuredStmt::CallHelper(Label(1))],
            helpers: vec![
                crate::lir_structure::StructuredHelper {
                    label: Label(1),
                    params: Vec::new(),
                    body: vec![
                        crate::lir_structure::StructuredStmt::Stmt(Stmt::Expr(Expr::Const(
                            ConstValue::Int(1),
                        ))),
                        crate::lir_structure::StructuredStmt::CallHelper(Label(2)),
                    ],
                },
                crate::lir_structure::StructuredHelper {
                    label: Label(2),
                    params: Vec::new(),
                    body: vec![
                        crate::lir_structure::StructuredStmt::Stmt(Stmt::Expr(Expr::Const(
                            ConstValue::Int(2),
                        ))),
                        crate::lir_structure::StructuredStmt::CallHelper(Label(1)),
                    ],
                },
            ],
            facts: Default::default(),
        };

        let error = super::validate_direct_helper_graph(&structured).unwrap_err().to_string();
        assert!(error.contains("helper cycle cannot be emitted as direct Python calls"), "{error}");
    }

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
        assert_no_trampoline_protocol(&emitted);
        assert!(emitted.contains("return _stale_capture_bb_1("), "{emitted}");
        assert!(emitted.contains(r#"_lir_outputs[0] = None"#), "{emitted}");

        let script = format!(
            "{emitted}\n\
             assert stale_capture(True) == [None], stale_capture(True)\n\
             assert stale_capture(False) == [9], stale_capture(False)\n"
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

    #[test]
    fn capture_of_definitely_defined_value_does_not_emit_undef_guard() {
        let value = LocalId(0);
        let function = Function {
            name: "defined_capture".to_owned(),
            params: Vec::new(),
            locals: vec![Local { id: value, name_hint: "value".to_owned(), ty: LirType::Int }],
            entry: Label(0),
            blocks: vec![Block {
                label: Label(0),
                stmts: vec![
                    Stmt::Assign { dst: value, value: Expr::Const(ConstValue::Int(11)) },
                    Stmt::Capture { key: "slot".to_owned(), value: Expr::Local(value) },
                ],
                term: Terminator::Return(Vec::new()),
            }],
            returns: Vec::new(),
        };

        let emitted = emit_function(&function).unwrap();
        assert_no_trampoline_protocol(&emitted);
        assert!(!emitted.contains("is not _LIR_UNDEF"), "{emitted}");
        assert!(!emitted.contains(r#"_lir_outputs[0] = None"#), "{emitted}");

        let script = format!("{emitted}\nassert defined_capture() == [11]\n");
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
