use std::collections::HashMap;

use crate::lir::{ConstValue, Expr, Function, Label, LocalId, ReturnValue, Stmt};
use crate::lir_structure::{StructuredFunction, StructuredStmt};

const CLEANUP_ROUNDS: usize = 4;
const FINAL_DCE_ROUNDS: usize = 1;
const INLINE_HELPER_STMT_LIMIT: usize = 12;
const PUSH_UP_HELPER_CALL_LIMIT: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackwardFacts {
    pub helper_live_ins: HashMap<Label, Vec<LocalId>>,
}

pub(crate) fn run_backward_passes(
    function: &Function,
    structured: &mut StructuredFunction,
) -> BackwardFacts {
    let mut facts = BackwardFacts::default();
    for _ in 0..CLEANUP_ROUNDS {
        let mut passes: Vec<Box<dyn BackwardLirPass>> = vec![
            Box::<HelperForwarding>::default(),
            Box::<SmallHelperInlining>::default(),
            Box::<StructuredSimplify>::default(),
            Box::<HelperComputationPushUp>::default(),
            Box::<HelperLiveIns>::default(),
            Box::<DeadAssignments>::default(),
        ];

        let mut changed = false;
        for pass in &mut passes {
            let mut cx = BackwardPassCx { function, structured, facts: &mut facts };
            changed |= pass.run(&mut cx);
        }

        if !changed {
            break;
        }
    }

    let mut structural_passes: Vec<Box<dyn BackwardLirPass>> = vec![
        Box::<HelperForwarding>::default(),
        Box::<SmallHelperInlining>::default(),
        Box::<StructuredSimplify>::default(),
        Box::<HelperComputationPushUp>::default(),
    ];
    for pass in &mut structural_passes {
        let mut cx = BackwardPassCx { function, structured, facts: &mut facts };
        pass.run(&mut cx);
    }

    for _ in 0..FINAL_DCE_ROUNDS {
        let mut passes: Vec<Box<dyn BackwardLirPass>> =
            vec![Box::<HelperLiveIns>::default(), Box::<DeadAssignments>::default()];

        let mut changed = false;
        for pass in &mut passes {
            let mut cx = BackwardPassCx { function, structured, facts: &mut facts };
            changed |= pass.run(&mut cx);
        }

        if !changed {
            break;
        }
    }

    let mut pass = Box::<HelperLiveIns>::default();
    let mut cx = BackwardPassCx { function, structured, facts: &mut facts };
    pass.run(&mut cx);

    facts
}

pub(crate) trait BackwardLirPass {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool;
}

pub(crate) struct BackwardPassCx<'a> {
    pub function: &'a Function,
    pub structured: &'a mut StructuredFunction,
    pub facts: &'a mut BackwardFacts,
}

impl BackwardPassCx<'_> {
    fn helper_params(&self, label: Label) -> Vec<LocalId> {
        self.facts.helper_live_ins.get(&label).cloned().unwrap_or_default()
    }

    fn update_helper_params(&mut self, label: Label, params: Vec<LocalId>) -> bool {
        let changed = self.facts.helper_live_ins.get(&label) != Some(&params);
        if changed {
            self.facts.helper_live_ins.insert(label, params.clone());
            if let Some(helper) =
                self.structured.helpers.iter_mut().find(|helper| helper.label == label)
            {
                helper.params = params;
            }
        }
        changed
    }
}

#[derive(Default)]
struct HelperForwarding;

impl BackwardLirPass for HelperForwarding {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let direct = cx
            .structured
            .helpers
            .iter()
            .filter_map(|helper| {
                helper_forward_target(&helper.body).map(|target| (helper.label, target))
            })
            .collect::<HashMap<_, _>>();

        let mut changed = false;
        for helper in &mut cx.structured.helpers {
            changed |= rewrite_helper_calls_in_body(&mut helper.body, &direct);
        }
        changed |= rewrite_helper_calls_in_body(&mut cx.structured.body, &direct);

        let before = cx.structured.helpers.len();
        cx.structured
            .helpers
            .retain(|helper| canonical_helper(helper.label, &direct) == helper.label);
        changed |= cx.structured.helpers.len() != before;

        changed
    }
}

fn helper_forward_target(body: &[StructuredStmt]) -> Option<Label> {
    match body {
        [StructuredStmt::CallHelper(label)] => Some(*label),
        [StructuredStmt::If { then_body, else_body, .. }] => {
            let then_target = terminal_helper_call(then_body)?;
            let else_target = terminal_helper_call(else_body)?;
            (then_target == else_target).then_some(then_target)
        }
        _ => None,
    }
}

fn terminal_helper_call(body: &[StructuredStmt]) -> Option<Label> {
    match body {
        [StructuredStmt::CallHelper(label)] => Some(*label),
        _ => None,
    }
}

fn rewrite_helper_calls_in_body(
    body: &mut Vec<StructuredStmt>,
    direct: &HashMap<Label, Label>,
) -> bool {
    let mut changed = false;
    let mut rewritten = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..) {
        changed |= rewrite_helper_calls_in_stmt(&mut stmt, direct);
        match stmt {
            StructuredStmt::If { then_body, else_body, .. } if then_body == else_body => {
                rewritten.extend(then_body);
                changed = true;
            }
            other => rewritten.push(other),
        }
    }

    *body = rewritten;
    changed
}

fn rewrite_helper_calls_in_stmt(stmt: &mut StructuredStmt, direct: &HashMap<Label, Label>) -> bool {
    match stmt {
        StructuredStmt::CallHelper(label) => {
            let canonical = canonical_helper(*label, direct);
            let changed = canonical != *label;
            *label = canonical;
            changed
        }
        StructuredStmt::If { then_body, else_body, .. } => {
            let then_changed = rewrite_helper_calls_in_body(then_body, direct);
            let else_changed = rewrite_helper_calls_in_body(else_body, direct);
            then_changed || else_changed
        }
        StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => false,
    }
}

fn canonical_helper(label: Label, direct: &HashMap<Label, Label>) -> Label {
    let mut current = label;
    let mut seen = Vec::new();
    while let Some(&next) = direct.get(&current) {
        if next == current || seen.contains(&next) {
            break;
        }
        seen.push(current);
        current = next;
    }
    current
}

#[derive(Default)]
struct SmallHelperInlining;

impl BackwardLirPass for SmallHelperInlining {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let call_counts = helper_call_counts(cx.structured);
        let inlineable = cx
            .structured
            .helpers
            .iter()
            .filter(|helper| {
                call_counts.get(&helper.label).copied().unwrap_or(0) == 1
                    && structured_stmt_count(&helper.body) <= INLINE_HELPER_STMT_LIMIT
                    && !body_calls_helper(&helper.body, helper.label)
            })
            .map(|helper| (helper.label, helper.body.clone()))
            .collect::<HashMap<_, _>>();

        if inlineable.is_empty() {
            return false;
        }

        let mut changed = false;
        changed |= inline_helper_calls_in_body(&mut cx.structured.body, &inlineable);
        for helper in &mut cx.structured.helpers {
            changed |= inline_helper_calls_in_body(&mut helper.body, &inlineable);
        }

        let used = helper_call_counts(cx.structured);
        let before = cx.structured.helpers.len();
        cx.structured.helpers.retain(|helper| used.contains_key(&helper.label));
        changed |= cx.structured.helpers.len() != before;
        changed
    }
}

#[derive(Default)]
struct StructuredSimplify;

impl BackwardLirPass for StructuredSimplify {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = simplify_body(&mut cx.structured.body);
        for helper in &mut cx.structured.helpers {
            changed |= simplify_body(&mut helper.body);
        }
        changed
    }
}

fn simplify_body(body: &mut Vec<StructuredStmt>) -> bool {
    let mut changed = false;
    let mut simplified = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..) {
        changed |= simplify_stmt(&mut stmt);
        match stmt {
            StructuredStmt::If { cond: Expr::Const(ConstValue::Bool(true)), then_body, .. } => {
                simplified.extend(then_body);
                changed = true;
            }
            StructuredStmt::If {
                cond: Expr::Const(ConstValue::Bool(false)), else_body, ..
            } => {
                simplified.extend(else_body);
                changed = true;
            }
            StructuredStmt::If { then_body, else_body, .. } if then_body == else_body => {
                simplified.extend(then_body);
                changed = true;
            }
            other => simplified.push(other),
        }
    }

    *body = simplified;
    changed
}

fn simplify_stmt(stmt: &mut StructuredStmt) -> bool {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            let then_changed = simplify_body(then_body);
            let else_changed = simplify_body(else_body);
            then_changed || else_changed
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    }
}

fn helper_call_counts(structured: &StructuredFunction) -> HashMap<Label, usize> {
    let mut counts = HashMap::new();
    count_helper_calls_in_body(&structured.body, &mut counts);
    for helper in &structured.helpers {
        count_helper_calls_in_body(&helper.body, &mut counts);
    }
    counts
}

fn count_helper_calls_in_body(body: &[StructuredStmt], counts: &mut HashMap<Label, usize>) {
    for stmt in body {
        count_helper_calls_in_stmt(stmt, counts);
    }
}

fn count_helper_calls_in_stmt(stmt: &StructuredStmt, counts: &mut HashMap<Label, usize>) {
    match stmt {
        StructuredStmt::CallHelper(label) => {
            *counts.entry(*label).or_insert(0) += 1;
        }
        StructuredStmt::If { then_body, else_body, .. } => {
            count_helper_calls_in_body(then_body, counts);
            count_helper_calls_in_body(else_body, counts);
        }
        StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => {}
    }
}

fn structured_stmt_count(body: &[StructuredStmt]) -> usize {
    body.iter().map(structured_stmt_weight).sum()
}

fn structured_stmt_weight(stmt: &StructuredStmt) -> usize {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            1 + structured_stmt_count(then_body) + structured_stmt_count(else_body)
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => 1,
    }
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

fn inline_helper_calls_in_body(
    body: &mut Vec<StructuredStmt>,
    inlineable: &HashMap<Label, Vec<StructuredStmt>>,
) -> bool {
    let mut changed = false;
    let mut inlined = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..) {
        match stmt {
            StructuredStmt::CallHelper(label) => {
                if let Some(replacement) = inlineable.get(&label) {
                    inlined.extend(replacement.clone());
                    changed = true;
                } else {
                    inlined.push(StructuredStmt::CallHelper(label));
                }
            }
            StructuredStmt::If { ref mut then_body, ref mut else_body, .. } => {
                changed |= inline_helper_calls_in_body(then_body, inlineable);
                changed |= inline_helper_calls_in_body(else_body, inlineable);
                inlined.push(stmt);
            }
            other => inlined.push(other),
        }
    }

    *body = inlined;
    changed
}

#[derive(Default)]
struct HelperComputationPushUp;

impl BackwardLirPass for HelperComputationPushUp {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let call_counts = helper_call_counts(cx.structured);
        let candidates = cx
            .structured
            .helpers
            .iter()
            .filter_map(|helper| {
                let call_count = call_counts.get(&helper.label).copied().unwrap_or(0);
                helper_push_up_candidate(helper.label, &helper.body, call_count)
            })
            .collect::<HashMap<_, _>>();

        if candidates.is_empty() {
            return false;
        }

        let mut changed = false;
        changed |= push_up_helper_computations_in_body(&mut cx.structured.body, &candidates);
        for helper in &mut cx.structured.helpers {
            changed |= push_up_helper_computations_in_body(&mut helper.body, &candidates);
        }

        for helper in &mut cx.structured.helpers {
            if let Some(candidate) = candidates.get(&helper.label) {
                if helper.body.first() == Some(&candidate.stmt) {
                    helper.body.remove(0);
                    changed = true;
                }
            }
        }

        changed
    }
}

#[derive(Clone, Debug)]
struct PushUpCandidate {
    stmt: StructuredStmt,
}

fn helper_push_up_candidate(
    label: Label,
    body: &[StructuredStmt],
    call_count: usize,
) -> Option<(Label, PushUpCandidate)> {
    if call_count == 0 || call_count > PUSH_UP_HELPER_CALL_LIMIT || body_calls_helper(body, label) {
        return None;
    }

    let StructuredStmt::Stmt(Stmt::Assign { dst, value }) = body.first()? else {
        return None;
    };
    if expr_has_side_effects(value) {
        return None;
    }

    let mut rhs_locals = Vec::new();
    collect_expr_local_ids(value, &mut rhs_locals);
    rhs_locals.sort();
    rhs_locals.dedup();
    if rhs_locals.len() < 2 || rhs_locals.contains(dst) {
        return None;
    }

    let mut tail_uses = Vec::new();
    for stmt in &body[1..] {
        collect_structured_stmt_uses(stmt, &mut tail_uses);
    }
    if !tail_uses.contains(dst) {
        return None;
    }

    let mut body_uses = Vec::new();
    for stmt in body {
        collect_structured_stmt_uses(stmt, &mut body_uses);
    }
    if rhs_locals.iter().any(|local| body_uses.iter().filter(|used| *used == local).count() > 1) {
        return None;
    }

    Some((
        label,
        PushUpCandidate {
            stmt: StructuredStmt::Stmt(Stmt::Assign { dst: *dst, value: value.clone() }),
        },
    ))
}

fn push_up_helper_computations_in_body(
    body: &mut Vec<StructuredStmt>,
    candidates: &HashMap<Label, PushUpCandidate>,
) -> bool {
    let mut changed = false;
    let mut rewritten = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..) {
        match stmt {
            StructuredStmt::CallHelper(label) => {
                if let Some(candidate) = candidates.get(&label) {
                    rewritten.push(candidate.stmt.clone());
                    changed = true;
                }
                rewritten.push(StructuredStmt::CallHelper(label));
            }
            StructuredStmt::If { ref mut then_body, ref mut else_body, .. } => {
                changed |= push_up_helper_computations_in_body(then_body, candidates);
                changed |= push_up_helper_computations_in_body(else_body, candidates);
                rewritten.push(stmt);
            }
            other => rewritten.push(other),
        }
    }

    *body = rewritten;
    changed
}

#[derive(Default)]
struct HelperLiveIns;

impl BackwardLirPass for HelperLiveIns {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let bodies = cx
            .structured
            .helpers
            .iter()
            .map(|helper| (helper.label, helper.body.clone()))
            .collect::<HashMap<_, _>>();
        let mut computed = HashMap::new();
        let mut visiting = Vec::new();
        let labels = cx.structured.helpers.iter().map(|helper| helper.label).collect::<Vec<_>>();
        let updates = labels
            .into_iter()
            .map(|label| {
                (
                    label,
                    compute_helper_params(
                        label,
                        cx.function.locals.len(),
                        &bodies,
                        &mut computed,
                        &mut visiting,
                    ),
                )
            })
            .collect::<Vec<_>>();

        let mut changed = false;
        for (label, params) in updates {
            changed |= cx.update_helper_params(label, params);
        }

        changed
    }
}

#[derive(Default)]
struct DeadAssignments;

impl BackwardLirPass for DeadAssignments {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = false;
        let locals_len = cx.function.locals.len();
        let helper_params = cx.facts.helper_live_ins.clone();

        for helper in &mut cx.structured.helpers {
            let mut live = LiveSet::new(locals_len);
            changed |= prune_dead_assignments_in_body(&mut helper.body, &mut live, &helper_params);
        }

        let mut live = LiveSet::new(locals_len);
        changed |=
            prune_dead_assignments_in_body(&mut cx.structured.body, &mut live, &helper_params);
        changed
    }
}

fn prune_dead_assignments_in_body(
    body: &mut Vec<StructuredStmt>,
    live: &mut LiveSet,
    helper_params: &HashMap<Label, Vec<LocalId>>,
) -> bool {
    let mut changed = false;
    let mut kept = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..).rev() {
        if prune_dead_assignments_in_stmt(&mut stmt, live, helper_params) {
            changed = true;
        }

        let keep = match &stmt {
            StructuredStmt::Stmt(Stmt::Assign { dst, value }) if !live.contains(*dst) => {
                if expr_has_side_effects(value) {
                    stmt = StructuredStmt::Stmt(Stmt::Expr(value.clone()));
                    true
                } else {
                    changed = true;
                    false
                }
            }
            _ => true,
        };

        if keep {
            apply_liveness_transfer(&stmt, live, helper_params);
            kept.push(stmt);
        }
    }

    kept.reverse();
    *body = kept;
    changed
}

fn prune_dead_assignments_in_stmt(
    stmt: &mut StructuredStmt,
    live: &mut LiveSet,
    helper_params: &HashMap<Label, Vec<LocalId>>,
) -> bool {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            let mut then_live = live.clone();
            let mut else_live = live.clone();
            let then_changed =
                prune_dead_assignments_in_body(then_body, &mut then_live, helper_params);
            let else_changed =
                prune_dead_assignments_in_body(else_body, &mut else_live, helper_params);
            then_live.union_with(&else_live);
            *live = then_live;
            then_changed || else_changed
        }
        _ => false,
    }
}

fn apply_liveness_transfer(
    stmt: &StructuredStmt,
    live: &mut LiveSet,
    helper_params: &HashMap<Label, Vec<LocalId>>,
) {
    match stmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value }) => {
            live.remove(*dst);
            collect_expr_locals(value, live);
        }
        StructuredStmt::Stmt(Stmt::Expr(value)) => collect_expr_locals(value, live),
        StructuredStmt::Stmt(Stmt::Unsupported { dsts, .. }) => {
            for dst in dsts {
                live.remove(*dst);
            }
        }
        StructuredStmt::If { cond, .. } => collect_expr_locals(cond, live),
        StructuredStmt::CallHelper(label) => {
            live.clear();
            for local in helper_params.get(label).into_iter().flatten() {
                live.insert(*local);
            }
        }
        StructuredStmt::Return(values) => {
            live.clear();
            for value in values {
                match value {
                    ReturnValue::Named { value, .. } => collect_expr_locals(value, live),
                }
            }
        }
        StructuredStmt::Raise(_) => live.clear(),
    }
}

fn expr_has_side_effects(expr: &Expr) -> bool {
    match expr {
        Expr::Call { .. } | Expr::Unsupported { .. } => true,
        Expr::Unary { arg, .. } => expr_has_side_effects(arg),
        Expr::Binary { lhs, rhs, .. } => expr_has_side_effects(lhs) || expr_has_side_effects(rhs),
        Expr::Local(_) | Expr::Const(_) => false,
    }
}

fn compute_helper_params(
    label: Label,
    locals_len: usize,
    bodies: &HashMap<Label, Vec<StructuredStmt>>,
    computed: &mut HashMap<Label, Vec<LocalId>>,
    visiting: &mut Vec<Label>,
) -> Vec<LocalId> {
    if let Some(params) = computed.get(&label) {
        return params.clone();
    }
    if visiting.contains(&label) {
        return Vec::new();
    }

    visiting.push(label);
    let live = bodies
        .get(&label)
        .map(|body| {
            live_in_body_static(
                body,
                LiveSet::new(locals_len),
                locals_len,
                bodies,
                computed,
                visiting,
            )
        })
        .unwrap_or_else(|| LiveSet::new(locals_len));
    visiting.pop();

    let params = live.into_sorted_locals();
    computed.insert(label, params.clone());
    params
}

fn live_in_body_static(
    body: &[StructuredStmt],
    mut live: LiveSet,
    locals_len: usize,
    bodies: &HashMap<Label, Vec<StructuredStmt>>,
    computed: &mut HashMap<Label, Vec<LocalId>>,
    visiting: &mut Vec<Label>,
) -> LiveSet {
    for stmt in body.iter().rev() {
        live = live_in_stmt_static(stmt, live, locals_len, bodies, computed, visiting);
    }
    live
}

fn live_in_stmt_static(
    stmt: &StructuredStmt,
    mut live: LiveSet,
    locals_len: usize,
    bodies: &HashMap<Label, Vec<StructuredStmt>>,
    computed: &mut HashMap<Label, Vec<LocalId>>,
    visiting: &mut Vec<Label>,
) -> LiveSet {
    match stmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value }) => {
            live.remove(*dst);
            collect_expr_locals(value, &mut live);
        }
        StructuredStmt::Stmt(Stmt::Expr(value)) => collect_expr_locals(value, &mut live),
        StructuredStmt::Stmt(Stmt::Unsupported { dsts, .. }) => {
            for dst in dsts {
                live.remove(*dst);
            }
        }
        StructuredStmt::If { cond, then_body, else_body } => {
            let mut then_live = live_in_body_static(
                then_body,
                live.clone(),
                locals_len,
                bodies,
                computed,
                visiting,
            );
            let else_live =
                live_in_body_static(else_body, live, locals_len, bodies, computed, visiting);
            then_live.union_with(&else_live);
            collect_expr_locals(cond, &mut then_live);
            live = then_live;
        }
        StructuredStmt::CallHelper(label) => {
            live = LiveSet::from_locals(
                locals_len,
                compute_helper_params(*label, locals_len, bodies, computed, visiting),
            );
        }
        StructuredStmt::Return(values) => {
            live.clear();
            for value in values {
                match value {
                    ReturnValue::Named { value, .. } => collect_expr_locals(value, &mut live),
                }
            }
        }
        StructuredStmt::Raise(_) => live.clear(),
    }
    live
}

impl BackwardPassCx<'_> {
    fn live_in_body(&self, body: &[StructuredStmt], mut live: LiveSet) -> LiveSet {
        for stmt in body.iter().rev() {
            live = self.live_in_stmt(stmt, live);
        }
        live
    }

    fn live_in_stmt(&self, stmt: &StructuredStmt, mut live: LiveSet) -> LiveSet {
        match stmt {
            StructuredStmt::Stmt(Stmt::Assign { dst, value }) => {
                live.remove(*dst);
                collect_expr_locals(value, &mut live);
            }
            StructuredStmt::Stmt(Stmt::Expr(value)) => collect_expr_locals(value, &mut live),
            StructuredStmt::Stmt(Stmt::Unsupported { dsts, .. }) => {
                for dst in dsts {
                    live.remove(*dst);
                }
            }
            StructuredStmt::If { cond, then_body, else_body } => {
                let mut then_live = self.live_in_body(then_body, live.clone());
                let else_live = self.live_in_body(else_body, live);
                then_live.union_with(&else_live);
                collect_expr_locals(cond, &mut then_live);
                live = then_live;
            }
            StructuredStmt::CallHelper(label) => {
                live = LiveSet::from_locals(self.function.locals.len(), self.helper_params(*label));
            }
            StructuredStmt::Return(values) => {
                live.clear();
                for value in values {
                    match value {
                        ReturnValue::Named { value, .. } => collect_expr_locals(value, &mut live),
                    }
                }
            }
            StructuredStmt::Raise(_) => live.clear(),
        }
        live
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveSet {
    bits: Vec<bool>,
}

impl LiveSet {
    fn new(len: usize) -> Self {
        Self { bits: vec![false; len] }
    }

    fn from_locals(len: usize, locals: Vec<LocalId>) -> Self {
        let mut set = Self::new(len);
        for local in locals {
            set.insert(local);
        }
        set
    }

    fn insert(&mut self, local: LocalId) {
        if let Some(slot) = self.bits.get_mut(local.0) {
            *slot = true;
        }
    }

    fn remove(&mut self, local: LocalId) {
        if let Some(slot) = self.bits.get_mut(local.0) {
            *slot = false;
        }
    }

    fn contains(&self, local: LocalId) -> bool {
        self.bits.get(local.0).copied().unwrap_or(false)
    }

    fn clear(&mut self) {
        self.bits.fill(false);
    }

    fn union_with(&mut self, other: &Self) {
        for (dst, src) in self.bits.iter_mut().zip(&other.bits) {
            *dst |= *src;
        }
    }

    fn into_sorted_locals(self) -> Vec<LocalId> {
        self.bits
            .into_iter()
            .enumerate()
            .filter_map(|(index, live)| live.then_some(LocalId(index)))
            .collect()
    }
}

fn collect_expr_locals(expr: &Expr, locals: &mut LiveSet) {
    match expr {
        Expr::Local(local) => {
            locals.insert(*local);
        }
        Expr::Const(_) => {}
        Expr::Unary { arg, .. } => collect_expr_locals(arg, locals),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_locals(lhs, locals);
            collect_expr_locals(rhs, locals);
        }
        Expr::Call { args, .. } | Expr::Unsupported { args, .. } => {
            for arg in args {
                collect_expr_locals(arg, locals);
            }
        }
    }
}

fn collect_structured_stmt_uses(stmt: &StructuredStmt, locals: &mut Vec<LocalId>) {
    match stmt {
        StructuredStmt::Stmt(Stmt::Assign { value, .. })
        | StructuredStmt::Stmt(Stmt::Expr(value)) => {
            collect_expr_local_ids(value, locals);
        }
        StructuredStmt::Stmt(Stmt::Unsupported { .. }) => {}
        StructuredStmt::If { cond, then_body, else_body } => {
            collect_expr_local_ids(cond, locals);
            for stmt in then_body {
                collect_structured_stmt_uses(stmt, locals);
            }
            for stmt in else_body {
                collect_structured_stmt_uses(stmt, locals);
            }
        }
        StructuredStmt::Return(values) => {
            for value in values {
                match value {
                    ReturnValue::Named { value, .. } => collect_expr_local_ids(value, locals),
                }
            }
        }
        StructuredStmt::CallHelper(_) | StructuredStmt::Raise(_) => {}
    }
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
