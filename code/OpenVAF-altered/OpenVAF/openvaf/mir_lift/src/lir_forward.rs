use std::collections::{HashMap, VecDeque};

use crate::lir::{BinaryOp, ConstValue, Expr, Function, Label, LocalId, Stmt, Terminator, UnaryOp};

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ForwardFacts {
    pub block_inputs: HashMap<Label, ConstEnv>,
}

type ConstEnv = Vec<Option<ConstValue>>;

pub(crate) fn run_forward_passes(function: Function) -> Function {
    let mut function = function;
    let mut passes: Vec<Box<dyn ForwardLirPass>> = vec![Box::<ConstantPropagation>::default()];
    for pass in &mut passes {
        pass.run(&mut function);
    }
    function
}

pub(crate) trait ForwardLirPass {
    fn run(&mut self, function: &mut Function);
}

#[derive(Default)]
struct ConstantPropagation;

impl ForwardLirPass for ConstantPropagation {
    fn run(&mut self, function: &mut Function) {
        let facts = constant_facts(function);
        rewrite_with_constants(function, &facts.block_inputs);
    }
}

fn constant_facts(function: &Function) -> ForwardFacts {
    let label_to_index = label_to_index(function);
    let preds = predecessors(function, &label_to_index);
    let succs = successors_by_block(function, &label_to_index);
    let env_len = function.locals.len();
    let unknown = || vec![None; env_len];

    let mut inputs = vec![unknown(); function.blocks.len()];
    let mut outputs = vec![unknown(); function.blocks.len()];
    let mut queued = vec![false; function.blocks.len()];
    let mut worklist = VecDeque::new();

    if let Some(entry) = label_to_index.get(&function.entry).copied() {
        worklist.push_back(entry);
        queued[entry] = true;
    }

    while let Some(index) = worklist.pop_front() {
        queued[index] = false;
        let input = if function.blocks[index].label == function.entry {
            unknown()
        } else {
            intersect_predecessors(&preds[index], &outputs, env_len)
        };

        inputs[index] = input.clone();
        let output = transfer_block(
            function.blocks[index].stmts.iter(),
            &function.blocks[index].term,
            input,
        );
        if outputs[index] == output {
            continue;
        }

        outputs[index] = output;
        for succ in &succs[index] {
            if !queued[*succ] {
                queued[*succ] = true;
                worklist.push_back(*succ);
            }
        }
    }

    ForwardFacts {
        block_inputs: function
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.label, inputs[index].clone()))
            .collect(),
    }
}

fn rewrite_with_constants(function: &mut Function, inputs: &HashMap<Label, ConstEnv>) {
    for block in &mut function.blocks {
        let mut env = inputs.get(&block.label).cloned().unwrap_or_default();
        for stmt in &mut block.stmts {
            rewrite_stmt(stmt, &mut env);
        }
        rewrite_terminator(&mut block.term, &env);
    }
}

fn rewrite_stmt(stmt: &mut Stmt, env: &mut ConstEnv) {
    match stmt {
        Stmt::Assign { dst, value } => {
            let rewritten = rewrite_expr(value.clone(), env);
            *value = rewritten.clone();
            if let Expr::Const(value) = rewritten {
                set_const(env, *dst, value);
            } else {
                clear_const(env, *dst);
            }
        }
        Stmt::Capture { value, .. } => *value = rewrite_expr(value.clone(), env),
        Stmt::Expr(value) => *value = rewrite_expr(value.clone(), env),
        Stmt::Unsupported { dsts, .. } => {
            for dst in dsts {
                clear_const(env, *dst);
            }
        }
    }
}

fn rewrite_terminator(term: &mut Terminator, env: &ConstEnv) {
    match term {
        Terminator::Branch { cond, then_label, else_label } => {
            let rewritten = rewrite_expr(cond.clone(), env);
            match rewritten {
                Expr::Const(ConstValue::Bool(true)) => *term = Terminator::Goto(*then_label),
                Expr::Const(ConstValue::Bool(false)) => *term = Terminator::Goto(*else_label),
                rewritten => *cond = rewritten,
            }
        }
        Terminator::Return(values) => {
            for value in values {
                match value {
                    crate::lir::ReturnValue::Named { value, .. } => {
                        *value = rewrite_expr(value.clone(), env);
                    }
                }
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

fn transfer_block<'a>(
    stmts: impl Iterator<Item = &'a Stmt>,
    term: &Terminator,
    mut env: ConstEnv,
) -> ConstEnv {
    for stmt in stmts {
        match stmt {
            Stmt::Assign { dst, value } => {
                let value = rewrite_expr(value.clone(), &env);
                if let Expr::Const(value) = value {
                    set_const(&mut env, *dst, value);
                } else {
                    clear_const(&mut env, *dst);
                }
            }
            Stmt::Capture { .. } => {}
            Stmt::Expr(_) => {}
            Stmt::Unsupported { dsts, .. } => {
                for dst in dsts {
                    clear_const(&mut env, *dst);
                }
            }
        }
    }

    if let Terminator::Return(values) = term {
        for value in values {
            match value {
                crate::lir::ReturnValue::Named { value, .. } => {
                    let _ = rewrite_expr(value.clone(), &env);
                }
            }
        }
    }

    env
}

fn rewrite_expr(expr: Expr, env: &ConstEnv) -> Expr {
    let expr = match expr {
        Expr::Local(local) => {
            return get_const(env, local).cloned().map(Expr::Const).unwrap_or(Expr::Local(local));
        }
        Expr::Unary { op, arg } => Expr::Unary { op, arg: Box::new(rewrite_expr(*arg, env)) },
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op,
            lhs: Box::new(rewrite_expr(*lhs, env)),
            rhs: Box::new(rewrite_expr(*rhs, env)),
        },
        Expr::Call { target, args } => Expr::Call {
            target,
            args: args.into_iter().map(|arg| rewrite_expr(arg, env)).collect(),
        },
        Expr::Unsupported { text, args } => Expr::Unsupported {
            text,
            args: args.into_iter().map(|arg| rewrite_expr(arg, env)).collect(),
        },
        Expr::Const(_) => expr,
    };
    fold_expr(expr)
}

fn fold_expr(expr: Expr) -> Expr {
    match expr {
        Expr::Unary { op, arg } => match *arg {
            Expr::Const(value) => fold_unary(op, value.clone())
                .map(Expr::Const)
                .unwrap_or_else(|| Expr::Unary { op, arg: Box::new(Expr::Const(value)) }),
            arg => Expr::Unary { op, arg: Box::new(arg) },
        },
        Expr::Binary { op, lhs, rhs } => match (*lhs, *rhs) {
            (Expr::Const(lhs), Expr::Const(rhs)) => fold_binary(op, lhs.clone(), rhs.clone())
                .map(Expr::Const)
                .unwrap_or_else(|| Expr::Binary {
                    op,
                    lhs: Box::new(Expr::Const(lhs)),
                    rhs: Box::new(Expr::Const(rhs)),
                }),
            (lhs, rhs) => Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
        },
        expr => expr,
    }
}

fn fold_unary(op: UnaryOp, value: ConstValue) -> Option<ConstValue> {
    match (op, value) {
        (UnaryOp::Not, ConstValue::Bool(value)) => Some(ConstValue::Bool(!value)),
        (UnaryOp::Neg, ConstValue::Int(value)) => Some(ConstValue::Int(-value)),
        (UnaryOp::Neg, ConstValue::Real(value)) => Some(ConstValue::Real(-value)),
        (UnaryOp::Cast(crate::lir::LirType::Bool), ConstValue::Bool(value)) => {
            Some(ConstValue::Bool(value))
        }
        (UnaryOp::Cast(crate::lir::LirType::Int), ConstValue::Int(value)) => {
            Some(ConstValue::Int(value))
        }
        (UnaryOp::Cast(crate::lir::LirType::Real), ConstValue::Real(value)) => {
            Some(ConstValue::Real(value))
        }
        (UnaryOp::Cast(crate::lir::LirType::Real), ConstValue::Int(value)) => {
            Some(ConstValue::Real(value.into()))
        }
        _ => None,
    }
}

fn fold_binary(op: BinaryOp, lhs: ConstValue, rhs: ConstValue) -> Option<ConstValue> {
    match (op, lhs, rhs) {
        (BinaryOp::Add, ConstValue::Int(lhs), ConstValue::Int(rhs)) => {
            lhs.checked_add(rhs).map(ConstValue::Int)
        }
        (BinaryOp::Sub, ConstValue::Int(lhs), ConstValue::Int(rhs)) => {
            lhs.checked_sub(rhs).map(ConstValue::Int)
        }
        (BinaryOp::Mul, ConstValue::Int(lhs), ConstValue::Int(rhs)) => {
            lhs.checked_mul(rhs).map(ConstValue::Int)
        }
        (BinaryOp::Div, ConstValue::Int(lhs), ConstValue::Int(rhs)) if rhs != 0 => {
            lhs.checked_div(rhs).map(ConstValue::Int)
        }
        (BinaryOp::Rem, ConstValue::Int(lhs), ConstValue::Int(rhs)) if rhs != 0 => {
            lhs.checked_rem(rhs).map(ConstValue::Int)
        }
        (BinaryOp::Add, ConstValue::Real(lhs), ConstValue::Real(rhs)) => {
            Some(ConstValue::Real(lhs + rhs))
        }
        (BinaryOp::Sub, ConstValue::Real(lhs), ConstValue::Real(rhs)) => {
            Some(ConstValue::Real(lhs - rhs))
        }
        (BinaryOp::Mul, ConstValue::Real(lhs), ConstValue::Real(rhs)) => {
            Some(ConstValue::Real(lhs * rhs))
        }
        (BinaryOp::Div, ConstValue::Real(lhs), ConstValue::Real(rhs)) if rhs != 0.0 => {
            Some(ConstValue::Real(lhs / rhs))
        }
        (BinaryOp::Eq, lhs, rhs) => Some(ConstValue::Bool(const_eq(&lhs, &rhs))),
        (BinaryOp::Ne, lhs, rhs) => Some(ConstValue::Bool(!const_eq(&lhs, &rhs))),
        (BinaryOp::Lt, ConstValue::Int(lhs), ConstValue::Int(rhs)) => {
            Some(ConstValue::Bool(lhs < rhs))
        }
        (BinaryOp::Le, ConstValue::Int(lhs), ConstValue::Int(rhs)) => {
            Some(ConstValue::Bool(lhs <= rhs))
        }
        (BinaryOp::Gt, ConstValue::Int(lhs), ConstValue::Int(rhs)) => {
            Some(ConstValue::Bool(lhs > rhs))
        }
        (BinaryOp::Ge, ConstValue::Int(lhs), ConstValue::Int(rhs)) => {
            Some(ConstValue::Bool(lhs >= rhs))
        }
        (BinaryOp::Lt, ConstValue::Real(lhs), ConstValue::Real(rhs)) => {
            Some(ConstValue::Bool(lhs < rhs))
        }
        (BinaryOp::Le, ConstValue::Real(lhs), ConstValue::Real(rhs)) => {
            Some(ConstValue::Bool(lhs <= rhs))
        }
        (BinaryOp::Gt, ConstValue::Real(lhs), ConstValue::Real(rhs)) => {
            Some(ConstValue::Bool(lhs > rhs))
        }
        (BinaryOp::Ge, ConstValue::Real(lhs), ConstValue::Real(rhs)) => {
            Some(ConstValue::Bool(lhs >= rhs))
        }
        _ => None,
    }
}

fn const_eq(lhs: &ConstValue, rhs: &ConstValue) -> bool {
    match (lhs, rhs) {
        (ConstValue::Bool(lhs), ConstValue::Bool(rhs)) => lhs == rhs,
        (ConstValue::Int(lhs), ConstValue::Int(rhs)) => lhs == rhs,
        (ConstValue::Real(lhs), ConstValue::Real(rhs)) => lhs == rhs,
        (ConstValue::Str(lhs), ConstValue::Str(rhs)) => lhs == rhs,
        (ConstValue::None, ConstValue::None) => true,
        _ => false,
    }
}

fn label_to_index(function: &Function) -> HashMap<Label, usize> {
    function.blocks.iter().enumerate().map(|(index, block)| (block.label, index)).collect()
}

fn predecessors(function: &Function, label_to_index: &HashMap<Label, usize>) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); function.blocks.len()];
    for (index, block) in function.blocks.iter().enumerate() {
        for succ in successor_labels(&block.term) {
            if let Some(succ) = label_to_index.get(&succ).copied() {
                preds[succ].push(index);
            }
        }
    }
    preds
}

fn successors_by_block(
    function: &Function,
    label_to_index: &HashMap<Label, usize>,
) -> Vec<Vec<usize>> {
    function
        .blocks
        .iter()
        .map(|block| {
            successor_labels(&block.term)
                .into_iter()
                .filter_map(|label| label_to_index.get(&label).copied())
                .collect()
        })
        .collect()
}

fn successor_labels(term: &Terminator) -> Vec<Label> {
    match term {
        Terminator::Goto(label) => vec![*label],
        Terminator::Branch { then_label, else_label, .. } => vec![*then_label, *else_label],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}

fn intersect_predecessors(preds: &[usize], outputs: &[ConstEnv], env_len: usize) -> ConstEnv {
    let Some((first, rest)) = preds.split_first() else {
        return vec![None; env_len];
    };
    let mut result = outputs[*first].clone();
    for pred in rest {
        let env = &outputs[*pred];
        for (value, other) in result.iter_mut().zip(env) {
            if !matches!((value.as_ref(), other.as_ref()), (Some(value), Some(other)) if const_eq(value, other))
            {
                *value = None;
            }
        }
    }
    result
}

fn get_const(env: &ConstEnv, local: LocalId) -> Option<&ConstValue> {
    env.get(local.0).and_then(Option::as_ref)
}

fn set_const(env: &mut ConstEnv, local: LocalId, value: ConstValue) {
    if let Some(slot) = env.get_mut(local.0) {
        *slot = Some(value);
    }
}

fn clear_const(env: &mut ConstEnv, local: LocalId) {
    if let Some(slot) = env.get_mut(local.0) {
        *slot = None;
    }
}
