use std::collections::{HashMap, VecDeque};

use crate::lir::{
    BinaryOp, CallEffect, ConstValue, Expr, Function, Label, LocalId, Stmt, Terminator, UnaryOp,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ForwardFacts {
    pub block_inputs: HashMap<Label, ConstEnv>,
}

type ConstEnv = Vec<Option<ConstValue>>;

const FORWARD_PASSES: &[ForwardPassKind] = &[
    ForwardPassKind::ConstantPropagation,
    ForwardPassKind::StrengthReduction,
    ForwardPassKind::CommonSubexpressions,
    ForwardPassKind::ConstantPropagation,
    ForwardPassKind::StrengthReduction,
];

pub(crate) fn run_forward_passes(function: Function) -> Function {
    let mut function = function;
    ForwardPipeline { name: "lir-forward", passes: FORWARD_PASSES }.run(&mut function);
    function
}

pub(crate) trait ForwardLirPass {
    fn run(&mut self, function: &mut Function);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForwardPassKind {
    ConstantPropagation,
    StrengthReduction,
    CommonSubexpressions,
}

impl ForwardPassKind {
    fn name(self) -> &'static str {
        match self {
            Self::ConstantPropagation => "constant-propagation",
            Self::StrengthReduction => "strength-reduction",
            Self::CommonSubexpressions => "common-subexpressions",
        }
    }

    fn run(self, function: &mut Function) {
        match self {
            Self::ConstantPropagation => run_forward_pass::<ConstantPropagation>(function),
            Self::StrengthReduction => run_forward_pass::<StrengthReduction>(function),
            Self::CommonSubexpressions => run_forward_pass::<CommonSubexpressions>(function),
        }
    }
}

struct ForwardPipeline {
    name: &'static str,
    passes: &'static [ForwardPassKind],
}

impl ForwardPipeline {
    fn run(&self, function: &mut Function) {
        let _pipeline_name = self.name;
        for pass in self.passes {
            let _pass_name = pass.name();
            pass.run(function);
        }
    }
}

fn run_forward_pass<P>(function: &mut Function)
where
    P: ForwardLirPass + Default,
{
    let mut pass = P::default();
    pass.run(function);
}

#[derive(Default)]
struct ConstantPropagation;

impl ForwardLirPass for ConstantPropagation {
    fn run(&mut self, function: &mut Function) {
        let facts = constant_facts(function);
        rewrite_with_constants(function, &facts.block_inputs);
    }
}

#[derive(Default)]
struct StrengthReduction;

impl ForwardLirPass for StrengthReduction {
    fn run(&mut self, function: &mut Function) {
        reduce_strength(function);
    }
}

#[derive(Default)]
struct CommonSubexpressions;

impl ForwardLirPass for CommonSubexpressions {
    fn run(&mut self, function: &mut Function) {
        eliminate_common_subexpressions(function);
    }
}

fn reduce_strength(function: &mut Function) {
    let local_types = local_types(function);
    for block in &mut function.blocks {
        for stmt in &mut block.stmts {
            reduce_strength_stmt(stmt, &local_types);
        }
        reduce_strength_terminator(&mut block.term, &local_types);
    }
}

fn reduce_strength_stmt(stmt: &mut Stmt, local_types: &[crate::lir::LirType]) {
    match stmt {
        Stmt::Assign { value, .. } | Stmt::Capture { value, .. } | Stmt::Expr(value) => {
            *value = reduce_strength_expr(value.clone(), local_types);
        }
        Stmt::CallEffect(effect) => reduce_strength_call_effect(effect, local_types),
        Stmt::Unsupported { .. } => {}
    }
}

fn reduce_strength_terminator(term: &mut Terminator, local_types: &[crate::lir::LirType]) {
    match term {
        Terminator::Branch { cond, .. } => {
            *cond = reduce_strength_expr(cond.clone(), local_types);
        }
        Terminator::Return(values) => {
            for value in values {
                match value {
                    crate::lir::ReturnValue::Named { value, .. } => {
                        *value = reduce_strength_expr(value.clone(), local_types);
                    }
                }
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

fn reduce_strength_call_effect(effect: &mut CallEffect, local_types: &[crate::lir::LirType]) {
    match effect {
        CallEffect::Diagnostic { args, .. } => {
            for arg in args {
                *arg = reduce_strength_expr(arg.clone(), local_types);
            }
        }
        CallEffect::SetInvalidParam { .. } | CallEffect::CollapseHint { .. } => {}
    }
}

fn reduce_strength_expr(expr: Expr, local_types: &[crate::lir::LirType]) -> Expr {
    let expr = match expr {
        Expr::Unary { op, arg } => {
            Expr::Unary { op, arg: Box::new(reduce_strength_expr(*arg, local_types)) }
        }
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op,
            lhs: Box::new(reduce_strength_expr(*lhs, local_types)),
            rhs: Box::new(reduce_strength_expr(*rhs, local_types)),
        },
        Expr::SimparamOpt { name, default } => Expr::SimparamOpt {
            name: Box::new(reduce_strength_expr(*name, local_types)),
            default: Box::new(reduce_strength_expr(*default, local_types)),
        },
        Expr::Call { target, args } => Expr::Call {
            target,
            args: args.into_iter().map(|arg| reduce_strength_expr(arg, local_types)).collect(),
        },
        Expr::Unsupported { text, args } => Expr::Unsupported {
            text,
            args: args.into_iter().map(|arg| reduce_strength_expr(arg, local_types)).collect(),
        },
        Expr::Local(_) | Expr::Const(_) => expr,
    };
    reduce_strength_root(expr, local_types)
}

fn reduce_strength_root(expr: Expr, local_types: &[crate::lir::LirType]) -> Expr {
    let Expr::Binary { op, lhs, rhs } = expr else {
        return expr;
    };

    match (op, *lhs, *rhs) {
        (BinaryOp::Add, lhs, Expr::Const(ConstValue::Int(0)))
        | (BinaryOp::Sub, lhs, Expr::Const(ConstValue::Int(0)))
            if expr_type(&lhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&lhs) =>
        {
            lhs
        }
        (BinaryOp::Add, Expr::Const(ConstValue::Int(0)), rhs)
            if expr_type(&rhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&rhs) =>
        {
            rhs
        }
        (BinaryOp::Mul, lhs, Expr::Const(ConstValue::Int(1)))
            if expr_type(&lhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&lhs) =>
        {
            lhs
        }
        (BinaryOp::Mul, Expr::Const(ConstValue::Int(1)), rhs)
            if expr_type(&rhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&rhs) =>
        {
            rhs
        }
        (BinaryOp::Mul, lhs, Expr::Const(ConstValue::Int(0)))
            if expr_type(&lhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&lhs) =>
        {
            Expr::Const(ConstValue::Int(0))
        }
        (BinaryOp::Mul, Expr::Const(ConstValue::Int(0)), rhs)
            if expr_type(&rhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&rhs) =>
        {
            Expr::Const(ConstValue::Int(0))
        }
        (BinaryOp::Shl, lhs, Expr::Const(ConstValue::Int(0)))
        | (BinaryOp::Shr, lhs, Expr::Const(ConstValue::Int(0)))
            if !expr_has_side_effects(&lhs) =>
        {
            lhs
        }
        (BinaryOp::BitAnd, lhs, Expr::Const(ConstValue::Int(-1)))
        | (BinaryOp::BitOr, lhs, Expr::Const(ConstValue::Int(0)))
        | (BinaryOp::BitXor, lhs, Expr::Const(ConstValue::Int(0)))
            if expr_type(&lhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&lhs) =>
        {
            lhs
        }
        (BinaryOp::BitAnd, Expr::Const(ConstValue::Int(-1)), rhs)
        | (BinaryOp::BitOr, Expr::Const(ConstValue::Int(0)), rhs)
        | (BinaryOp::BitXor, Expr::Const(ConstValue::Int(0)), rhs)
            if expr_type(&rhs, local_types) == Some(crate::lir::LirType::Int)
                && !expr_has_side_effects(&rhs) =>
        {
            rhs
        }
        (BinaryOp::Mul, lhs, Expr::Const(ConstValue::Int(value)))
            if expr_type(&lhs, local_types) == Some(crate::lir::LirType::Int) =>
        {
            multiply_by_power_of_two(&lhs, value).unwrap_or_else(|| {
                binary_expr(BinaryOp::Mul, lhs, Expr::Const(ConstValue::Int(value)))
            })
        }
        (BinaryOp::Mul, Expr::Const(ConstValue::Int(value)), rhs)
            if expr_type(&rhs, local_types) == Some(crate::lir::LirType::Int) =>
        {
            multiply_by_power_of_two(&rhs, value).unwrap_or_else(|| {
                binary_expr(BinaryOp::Mul, Expr::Const(ConstValue::Int(value)), rhs)
            })
        }
        (op, lhs, rhs) => binary_expr(op, lhs, rhs),
    }
}

fn multiply_by_power_of_two(expr: &Expr, value: i32) -> Option<Expr> {
    if expr_has_side_effects(expr) || value <= 1 || value & (value - 1) != 0 {
        return None;
    }
    Some(binary_expr(
        BinaryOp::Shl,
        expr.clone(),
        Expr::Const(ConstValue::Int(value.trailing_zeros() as i32)),
    ))
}

fn binary_expr(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }
}

fn eliminate_common_subexpressions(function: &mut Function) {
    let locals_len = function.locals.len();
    for block in &mut function.blocks {
        eliminate_block_common_subexpressions(block, locals_len);
    }
}

fn eliminate_block_common_subexpressions(block: &mut crate::lir::Block, locals_len: usize) {
    let mut versions = vec![0usize; locals_len];
    let mut available = HashMap::new();

    for stmt in &mut block.stmts {
        eliminate_stmt_common_subexpressions(stmt, &mut versions, &mut available);
    }
    eliminate_terminator_common_subexpressions(&mut block.term, &versions, &available);
}

fn eliminate_stmt_common_subexpressions(
    stmt: &mut Stmt,
    versions: &mut [usize],
    available: &mut HashMap<ExprKey, LocalId>,
) {
    match stmt {
        Stmt::Assign { dst, value } => {
            let rewritten = rewrite_common_subexpressions(value.clone(), versions, available);
            let available_key =
                expr_key(&rewritten, versions).filter(|_| expr_is_cse_candidate(&rewritten));
            *value = if let Some(key) = &available_key {
                available.get(key).copied().map(Expr::Local).unwrap_or(rewritten)
            } else {
                rewritten
            };
            bump_version(versions, *dst);
            if let Some(key) = available_key {
                available.entry(key).or_insert(*dst);
            }
        }
        Stmt::Capture { value, .. } | Stmt::Expr(value) => {
            *value = rewrite_common_subexpressions(value.clone(), versions, available);
        }
        Stmt::CallEffect(effect) => {
            rewrite_call_effect_common_subexpressions(effect, versions, available)
        }
        Stmt::Unsupported { dsts, .. } => {
            for dst in dsts {
                bump_version(versions, *dst);
            }
        }
    }
}

fn eliminate_terminator_common_subexpressions(
    term: &mut Terminator,
    versions: &[usize],
    available: &HashMap<ExprKey, LocalId>,
) {
    match term {
        Terminator::Branch { cond, .. } => {
            *cond = rewrite_common_subexpressions(cond.clone(), versions, available);
        }
        Terminator::Return(values) => {
            for value in values {
                match value {
                    crate::lir::ReturnValue::Named { value, .. } => {
                        *value = rewrite_common_subexpressions(value.clone(), versions, available);
                    }
                }
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

fn rewrite_call_effect_common_subexpressions(
    effect: &mut CallEffect,
    versions: &[usize],
    available: &HashMap<ExprKey, LocalId>,
) {
    match effect {
        CallEffect::Diagnostic { args, .. } => {
            for arg in args {
                *arg = rewrite_common_subexpressions(arg.clone(), versions, available);
            }
        }
        CallEffect::SetInvalidParam { .. } | CallEffect::CollapseHint { .. } => {}
    }
}

fn rewrite_common_subexpressions(
    expr: Expr,
    versions: &[usize],
    available: &HashMap<ExprKey, LocalId>,
) -> Expr {
    let rewritten = match expr {
        Expr::Unary { op, arg } => Expr::Unary {
            op,
            arg: Box::new(rewrite_common_subexpressions(*arg, versions, available)),
        },
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op,
            lhs: Box::new(rewrite_common_subexpressions(*lhs, versions, available)),
            rhs: Box::new(rewrite_common_subexpressions(*rhs, versions, available)),
        },
        Expr::SimparamOpt { name, default } => Expr::SimparamOpt {
            name: Box::new(rewrite_common_subexpressions(*name, versions, available)),
            default: Box::new(rewrite_common_subexpressions(*default, versions, available)),
        },
        Expr::Call { target, args } => Expr::Call {
            target,
            args: args
                .into_iter()
                .map(|arg| rewrite_common_subexpressions(arg, versions, available))
                .collect(),
        },
        Expr::Unsupported { text, args } => Expr::Unsupported {
            text,
            args: args
                .into_iter()
                .map(|arg| rewrite_common_subexpressions(arg, versions, available))
                .collect(),
        },
        Expr::Local(_) | Expr::Const(_) => expr,
    };

    expr_key(&rewritten, versions)
        .filter(|_| expr_is_cse_candidate(&rewritten))
        .and_then(|key| available.get(&key).copied())
        .map(Expr::Local)
        .unwrap_or(rewritten)
}

fn bump_version(versions: &mut [usize], local: LocalId) {
    if let Some(version) = versions.get_mut(local.0) {
        *version += 1;
    }
}

fn expr_is_cse_candidate(expr: &Expr) -> bool {
    !matches!(expr, Expr::Local(_) | Expr::Const(_)) && !expr_has_side_effects(expr)
}

fn expr_has_side_effects(expr: &Expr) -> bool {
    match expr {
        Expr::Call { .. } | Expr::Unsupported { .. } => true,
        Expr::Unary { arg, .. } => expr_has_side_effects(arg),
        Expr::Binary { lhs, rhs, .. } => expr_has_side_effects(lhs) || expr_has_side_effects(rhs),
        Expr::SimparamOpt { name, default } => {
            expr_has_side_effects(name) || expr_has_side_effects(default)
        }
        Expr::Local(_) | Expr::Const(_) => false,
    }
}

fn local_types(function: &Function) -> Vec<crate::lir::LirType> {
    function.locals.iter().map(|local| local.ty).collect()
}

fn expr_type(expr: &Expr, local_types: &[crate::lir::LirType]) -> Option<crate::lir::LirType> {
    match expr {
        Expr::Local(local) => local_types.get(local.0).copied(),
        Expr::Const(ConstValue::Bool(_)) => Some(crate::lir::LirType::Bool),
        Expr::Const(ConstValue::Int(_)) => Some(crate::lir::LirType::Int),
        Expr::Const(ConstValue::Real(_)) => Some(crate::lir::LirType::Real),
        Expr::Const(ConstValue::Str(_)) => Some(crate::lir::LirType::Str),
        Expr::Const(ConstValue::None) => None,
        Expr::Unary { op: UnaryOp::Not, .. } => Some(crate::lir::LirType::Bool),
        Expr::Unary { op: UnaryOp::Neg, arg } => expr_type(arg, local_types),
        Expr::Unary { op: UnaryOp::Cast(ty), .. } => Some(*ty),
        Expr::Unary { op: UnaryOp::Math1(_), .. } => Some(crate::lir::LirType::Real),
        Expr::Binary { op, lhs, .. } => match op {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor => expr_type(lhs, local_types),
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => Some(crate::lir::LirType::Bool),
            BinaryOp::Math2(_) => Some(crate::lir::LirType::Real),
        },
        Expr::SimparamOpt { default, .. } => expr_type(default, local_types),
        Expr::Call { .. } | Expr::Unsupported { .. } => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ExprKey {
    Local(LocalId, usize),
    Const(ConstKey),
    Unary { op: UnaryOp, arg: Box<ExprKey> },
    Binary { op: BinaryOp, lhs: Box<ExprKey>, rhs: Box<ExprKey> },
    SimparamOpt { name: Box<ExprKey>, default: Box<ExprKey> },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ConstKey {
    Bool(bool),
    Int(i32),
    Real(u64),
    Str(String),
    None,
}

fn expr_key(expr: &Expr, versions: &[usize]) -> Option<ExprKey> {
    match expr {
        Expr::Local(local) => Some(ExprKey::Local(*local, versions.get(local.0).copied()?)),
        Expr::Const(value) => Some(ExprKey::Const(const_key(value))),
        Expr::Unary { op, arg } => {
            Some(ExprKey::Unary { op: *op, arg: Box::new(expr_key(arg, versions)?) })
        }
        Expr::Binary { op, lhs, rhs } => Some(ExprKey::Binary {
            op: *op,
            lhs: Box::new(expr_key(lhs, versions)?),
            rhs: Box::new(expr_key(rhs, versions)?),
        }),
        Expr::SimparamOpt { name, default } => Some(ExprKey::SimparamOpt {
            name: Box::new(expr_key(name, versions)?),
            default: Box::new(expr_key(default, versions)?),
        }),
        Expr::Call { .. } | Expr::Unsupported { .. } => None,
    }
}

fn const_key(value: &ConstValue) -> ConstKey {
    match value {
        ConstValue::Bool(value) => ConstKey::Bool(*value),
        ConstValue::Int(value) => ConstKey::Int(*value),
        ConstValue::Real(value) => ConstKey::Real(value.to_bits()),
        ConstValue::Str(value) => ConstKey::Str(value.clone()),
        ConstValue::None => ConstKey::None,
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
        Stmt::CallEffect(effect) => rewrite_call_effect(effect, env),
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
            Stmt::CallEffect(_) => {}
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
        Expr::SimparamOpt { name, default } => Expr::SimparamOpt {
            name: Box::new(rewrite_expr(*name, env)),
            default: Box::new(rewrite_expr(*default, env)),
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

fn rewrite_call_effect(effect: &mut CallEffect, env: &ConstEnv) {
    match effect {
        CallEffect::Diagnostic { args, .. } => {
            for arg in args {
                *arg = rewrite_expr(arg.clone(), env);
            }
        }
        CallEffect::SetInvalidParam { .. } | CallEffect::CollapseHint { .. } => {}
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::{Block, LirType, Local, ReturnValue};

    #[test]
    fn strength_reduction_simplifies_integer_identities_and_power_of_two_multiply() {
        let mut function = test_function();
        function.blocks[0].stmts = vec![
            Stmt::Assign {
                dst: LocalId(2),
                value: binary_expr(
                    BinaryOp::Add,
                    Expr::Local(LocalId(0)),
                    Expr::Const(ConstValue::Int(0)),
                ),
            },
            Stmt::Assign {
                dst: LocalId(3),
                value: binary_expr(
                    BinaryOp::Mul,
                    Expr::Local(LocalId(0)),
                    Expr::Const(ConstValue::Int(8)),
                ),
            },
        ];

        reduce_strength(&mut function);

        assert_eq!(function.blocks[0].stmts[0], assign(LocalId(2), Expr::Local(LocalId(0))));
        assert_eq!(
            function.blocks[0].stmts[1],
            assign(
                LocalId(3),
                binary_expr(
                    BinaryOp::Shl,
                    Expr::Local(LocalId(0)),
                    Expr::Const(ConstValue::Int(3))
                ),
            )
        );
    }

    #[test]
    fn strength_reduction_keeps_effectful_operands() {
        let mut function = test_function();
        let call = Expr::Call { target: "next".to_owned(), args: Vec::new() };
        function.blocks[0].stmts = vec![Stmt::Assign {
            dst: LocalId(2),
            value: binary_expr(BinaryOp::Add, call.clone(), Expr::Const(ConstValue::Int(0))),
        }];

        reduce_strength(&mut function);

        assert_eq!(
            function.blocks[0].stmts[0],
            assign(LocalId(2), binary_expr(BinaryOp::Add, call, Expr::Const(ConstValue::Int(0))))
        );
    }

    #[test]
    fn common_subexpressions_reuse_available_pure_expression() {
        let mut function = test_function();
        let sum = binary_expr(BinaryOp::Add, Expr::Local(LocalId(0)), Expr::Local(LocalId(1)));
        function.blocks[0].stmts = vec![assign(LocalId(2), sum.clone()), assign(LocalId(3), sum)];

        eliminate_common_subexpressions(&mut function);

        assert_eq!(
            function.blocks[0].stmts,
            vec![
                assign(
                    LocalId(2),
                    binary_expr(BinaryOp::Add, Expr::Local(LocalId(0)), Expr::Local(LocalId(1))),
                ),
                assign(LocalId(3), Expr::Local(LocalId(2))),
            ]
        );
    }

    #[test]
    fn common_subexpressions_respect_redefinitions() {
        let mut function = test_function();
        let sum = binary_expr(BinaryOp::Add, Expr::Local(LocalId(0)), Expr::Local(LocalId(1)));
        function.blocks[0].stmts = vec![
            assign(LocalId(2), sum.clone()),
            assign(LocalId(0), Expr::Const(ConstValue::Int(4))),
            assign(LocalId(3), sum.clone()),
        ];

        eliminate_common_subexpressions(&mut function);

        assert_eq!(
            function.blocks[0].stmts,
            vec![
                assign(LocalId(2), sum.clone()),
                assign(LocalId(0), Expr::Const(ConstValue::Int(4))),
                assign(LocalId(3), sum),
            ]
        );
    }

    fn assign(dst: LocalId, value: Expr) -> Stmt {
        Stmt::Assign { dst, value }
    }

    fn test_function() -> Function {
        Function {
            name: "test".to_owned(),
            params: vec![LocalId(0), LocalId(1)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "a".to_owned(), ty: LirType::Int },
                Local { id: LocalId(1), name_hint: "b".to_owned(), ty: LirType::Int },
                Local { id: LocalId(2), name_hint: "c".to_owned(), ty: LirType::Int },
                Local { id: LocalId(3), name_hint: "d".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: vec![Block {
                label: Label(0),
                stmts: Vec::new(),
                term: Terminator::Return(vec![ReturnValue::Named {
                    key: "out".to_owned(),
                    value: Expr::Local(LocalId(3)),
                }]),
            }],
            returns: Vec::new(),
            output_types: HashMap::new(),
        }
    }
}
