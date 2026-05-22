use std::collections::{HashMap, HashSet};

use crate::lir::{CallEffect, ConstValue, Expr, Function, Label, LocalId, ReturnValue, Stmt};
use crate::lir_structure::{StructuredFunction, StructuredStmt};

const CLEANUP_ROUNDS: usize = 4;
const FINAL_DCE_ROUNDS: usize = 3;
const POST_DCE_CLEANUP_ROUNDS: usize = 0;
const SMALL_HELPER_STMT_LIMIT: usize = 12;
const COST_INLINE_HELPER_CALL_LIMIT: usize = 3;
const COST_INLINE_MIN_SAVINGS: usize = 32;
const PUSH_UP_HELPER_CALL_LIMIT: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackwardFacts {
    pub helper_live_ins: HashMap<Label, Vec<LocalId>>,
    pub optional_helper_live_ins: HashSet<(Label, LocalId)>,
}

const DISABLED_PASSES: &[BackwardPassKind] = &[BackwardPassKind::HelperLiveIns];
const CLEANUP_PASSES: &[BackwardPassKind] = &[
    BackwardPassKind::HelperForwarding,
    BackwardPassKind::CommonTailHelperSinking,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CostBasedHelperInlining,
    BackwardPassKind::StructuredSimplify,
    BackwardPassKind::HelperComputationPushUp,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::HelperSignaturePruning,
];
const STRUCTURAL_PASSES: &[BackwardPassKind] = &[
    BackwardPassKind::HelperForwarding,
    BackwardPassKind::CommonTailHelperSinking,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CostBasedHelperInlining,
    BackwardPassKind::StructuredSimplify,
    BackwardPassKind::HelperComputationPushUp,
    BackwardPassKind::HelperSignaturePruning,
];
const FINAL_DCE_PASSES: &[BackwardPassKind] = &[
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::HelperSignaturePruning,
];
const POST_DCE_CLEANUP_PASSES: &[BackwardPassKind] = CLEANUP_PASSES;
const FINALIZATION_PASSES: &[BackwardPassKind] =
    &[BackwardPassKind::HelperSignaturePruning, BackwardPassKind::OptionalHelperLiveIns];

pub(crate) fn run_backward_passes(
    function: &Function,
    structured: &mut StructuredFunction,
) -> BackwardFacts {
    let mut facts = BackwardFacts::default();
    if std::env::var_os("MIR_LIFT_DISABLE_LIR_OPTS").is_some() {
        BackwardPipeline { name: "lir-backward-disabled", passes: DISABLED_PASSES }
            .run(&mut BackwardPassCx { function, structured, facts: &mut facts });
        return facts;
    }

    let cleanup_pipeline =
        BackwardPipeline { name: "lir-backward-cleanup", passes: CLEANUP_PASSES };
    let structural_pipeline =
        BackwardPipeline { name: "lir-backward-structural", passes: STRUCTURAL_PASSES };
    let final_dce_pipeline =
        BackwardPipeline { name: "lir-backward-final-dce", passes: FINAL_DCE_PASSES };

    for _ in 0..CLEANUP_ROUNDS {
        if !cleanup_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts }) {
            break;
        }
    }

    structural_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts });

    for _ in 0..FINAL_DCE_ROUNDS {
        if !final_dce_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts })
        {
            break;
        }
    }

    for _ in 0..POST_DCE_CLEANUP_ROUNDS {
        if !run_metric_guarded_post_dce_cleanup(function, structured, &mut facts) {
            break;
        }
    }

    BackwardPipeline { name: "lir-backward-finalization", passes: FINALIZATION_PASSES }
        .run(&mut BackwardPassCx { function, structured, facts: &mut facts });

    log_helper_stats(function, structured, &facts);
    facts
}

pub(crate) trait BackwardLirPass {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackwardPassKind {
    HelperForwarding,
    CommonTailHelperSinking,
    HelperSignaturePruning,
    CostBasedHelperInlining,
    StructuredSimplify,
    HelperComputationPushUp,
    HelperLiveIns,
    OptionalHelperLiveIns,
    DeadAssignments,
}

impl BackwardPassKind {
    fn name(self) -> &'static str {
        match self {
            Self::HelperForwarding => "helper-forwarding",
            Self::CommonTailHelperSinking => "common-tail-helper-sinking",
            Self::HelperSignaturePruning => "helper-signature-pruning",
            Self::CostBasedHelperInlining => "cost-based-helper-inlining",
            Self::StructuredSimplify => "structured-simplify",
            Self::HelperComputationPushUp => "helper-computation-push-up",
            Self::HelperLiveIns => "helper-live-ins",
            Self::OptionalHelperLiveIns => "optional-helper-live-ins",
            Self::DeadAssignments => "dead-assignments",
        }
    }

    fn run(self, cx: &mut BackwardPassCx<'_>) -> bool {
        match self {
            Self::HelperForwarding => run_backward_pass::<HelperForwarding>(cx),
            Self::CommonTailHelperSinking => run_backward_pass::<CommonTailHelperSinking>(cx),
            Self::HelperSignaturePruning => run_backward_pass::<HelperSignaturePruning>(cx),
            Self::CostBasedHelperInlining => run_backward_pass::<CostBasedHelperInlining>(cx),
            Self::StructuredSimplify => run_backward_pass::<StructuredSimplify>(cx),
            Self::HelperComputationPushUp => run_backward_pass::<HelperComputationPushUp>(cx),
            Self::HelperLiveIns => run_backward_pass::<HelperLiveIns>(cx),
            Self::OptionalHelperLiveIns => run_backward_pass::<OptionalHelperLiveIns>(cx),
            Self::DeadAssignments => run_backward_pass::<DeadAssignments>(cx),
        }
    }
}

struct BackwardPipeline {
    name: &'static str,
    passes: &'static [BackwardPassKind],
}

impl BackwardPipeline {
    fn run(&self, cx: &mut BackwardPassCx<'_>) -> bool {
        let _pipeline_name = self.name;
        let mut changed = false;
        for pass in self.passes {
            let _pass_name = pass.name();
            changed |= pass.run(cx);
        }
        changed
    }
}

fn run_backward_pass<P>(cx: &mut BackwardPassCx<'_>) -> bool
where
    P: BackwardLirPass + Default,
{
    let mut pass = P::default();
    pass.run(cx)
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
struct CommonTailHelperSinking;

impl BackwardLirPass for CommonTailHelperSinking {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = sink_common_tail_helpers_in_body(&mut cx.structured.body);
        for helper in &mut cx.structured.helpers {
            changed |= sink_common_tail_helpers_in_body(&mut helper.body);
        }
        changed
    }
}

fn sink_common_tail_helpers_in_body(body: &mut Vec<StructuredStmt>) -> bool {
    let mut changed = false;
    let mut rewritten = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..) {
        changed |= sink_common_tail_helpers_in_stmt(&mut stmt);
        match stmt {
            StructuredStmt::If { cond, mut then_body, mut else_body } => {
                match common_tail_helper(&then_body, &else_body) {
                    Some(label) if can_sink_tail_helper(&then_body, &else_body) => {
                        then_body.pop();
                        else_body.pop();
                        rewritten.push(StructuredStmt::If { cond, then_body, else_body });
                        rewritten.push(StructuredStmt::CallHelper(label));
                        changed = true;
                    }
                    _ => {
                        rewritten.push(StructuredStmt::If { cond, then_body, else_body });
                    }
                }
            }
            other => rewritten.push(other),
        }
    }

    *body = rewritten;
    changed
}

fn sink_common_tail_helpers_in_stmt(stmt: &mut StructuredStmt) -> bool {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            let then_changed = sink_common_tail_helpers_in_body(then_body);
            let else_changed = sink_common_tail_helpers_in_body(else_body);
            then_changed || else_changed
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    }
}

fn common_tail_helper(then_body: &[StructuredStmt], else_body: &[StructuredStmt]) -> Option<Label> {
    match (then_body.last(), else_body.last()) {
        (
            Some(StructuredStmt::CallHelper(then_label)),
            Some(StructuredStmt::CallHelper(else_label)),
        ) if then_label == else_label => Some(*then_label),
        _ => None,
    }
}

fn can_sink_tail_helper(then_body: &[StructuredStmt], else_body: &[StructuredStmt]) -> bool {
    let then_len = then_body.len();
    let else_len = else_body.len();
    then_len > 1 && else_len > 1 || then_len == 1 && else_len == 1
}

#[derive(Default)]
struct CostBasedHelperInlining;

impl BackwardLirPass for CostBasedHelperInlining {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let call_counts = helper_call_counts(cx.structured);
        let safe_call_counts =
            safe_helper_call_counts(cx.function, cx.structured, &cx.facts.helper_live_ins);
        let candidates = cx
            .structured
            .helpers
            .iter()
            .filter_map(|helper| {
                let call_count = call_counts.get(&helper.label).copied().unwrap_or(0);
                let safe_call_count = safe_call_counts.get(&helper.label).copied().unwrap_or(0);
                let params = cx.helper_params(helper.label);
                helper_inline_candidate(
                    cx.function,
                    helper.label,
                    &helper.body,
                    &params,
                    &cx.facts.helper_live_ins,
                    call_count,
                    safe_call_count,
                )
            })
            .collect::<HashMap<_, _>>();

        if candidates.is_empty() {
            return false;
        }

        if std::env::var_os("MIR_LIFT_TIMING").is_some() {
            let avoided_params = candidates
                .keys()
                .map(|label| {
                    cx.helper_params(*label).len()
                        * (call_counts.get(label).copied().unwrap_or(0) + 1)
                })
                .sum::<usize>();
            eprintln!(
                "mir-lift timing: {} cost-inline-helpers count={} avoided_param_slots={}",
                cx.function.name,
                candidates.len(),
                avoided_params
            );
        }

        let inlineable = candidates
            .into_iter()
            .map(|(label, candidate)| (label, candidate.body))
            .collect::<HashMap<_, _>>();
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

#[derive(Clone, Debug)]
struct InlineCandidate {
    body: Vec<StructuredStmt>,
}

fn helper_inline_candidate(
    function: &Function,
    label: Label,
    body: &[StructuredStmt],
    params: &[LocalId],
    helper_params: &HashMap<Label, Vec<LocalId>>,
    call_count: usize,
    safe_call_count: usize,
) -> Option<(Label, InlineCandidate)> {
    if call_count == 0 || call_count != safe_call_count || body_calls_helper(body, label) {
        return None;
    }

    let stmt_count = structured_stmt_count(body);
    let body_cost = structured_body_text_cost(function, body, helper_params);
    let boundary_cost = helper_boundary_cost(function, params, call_count);
    let duplicated_body_cost = call_count.saturating_sub(1) * body_cost;
    let estimated_savings = boundary_cost.saturating_sub(duplicated_body_cost);
    let inline = if call_count == 1 {
        estimated_savings >= COST_INLINE_MIN_SAVINGS || stmt_count <= SMALL_HELPER_STMT_LIMIT
    } else if call_count <= COST_INLINE_HELPER_CALL_LIMIT && linear_helper_body(body) {
        estimated_savings >= COST_INLINE_MIN_SAVINGS
    } else {
        false
    };

    inline.then(|| (label, InlineCandidate { body: body.to_vec() }))
}

fn helper_boundary_cost(function: &Function, params: &[LocalId], call_count: usize) -> usize {
    helper_def_text_cost(function, params) + call_count * helper_call_text_cost(function, params)
}

fn helper_def_text_cost(function: &Function, params: &[LocalId]) -> usize {
    12 + params_text_cost(function, params)
}

fn helper_call_text_cost(function: &Function, params: &[LocalId]) -> usize {
    28 + params_text_cost(function, params)
}

fn params_text_cost(function: &Function, params: &[LocalId]) -> usize {
    params.iter().map(|local| local_text_cost(function, *local) + 2).sum()
}

fn local_text_cost(function: &Function, local: LocalId) -> usize {
    function
        .local(local)
        .map(|local| sanitize_ident_len(&local.name_hint))
        .unwrap_or_else(|| 1 + digits(local.0))
}

fn sanitize_ident_len(name: &str) -> usize {
    let len = name.chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_').count().max(1);
    if name.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        len + 1
    } else {
        len
    }
}

fn digits(mut value: usize) -> usize {
    let mut count = 1;
    while value >= 10 {
        value /= 10;
        count += 1;
    }
    count
}

fn structured_body_text_cost(
    function: &Function,
    body: &[StructuredStmt],
    helper_params: &HashMap<Label, Vec<LocalId>>,
) -> usize {
    body.iter().map(|stmt| structured_stmt_text_cost(function, stmt, helper_params)).sum()
}

fn structured_stmt_text_cost(
    function: &Function,
    stmt: &StructuredStmt,
    helper_params: &HashMap<Label, Vec<LocalId>>,
) -> usize {
    match stmt {
        StructuredStmt::Stmt(stmt) => stmt_text_cost(function, stmt),
        StructuredStmt::If { cond, then_body, else_body } => {
            16 + expr_text_cost(function, cond)
                + structured_body_text_cost(function, then_body, helper_params)
                + structured_body_text_cost(function, else_body, helper_params)
        }
        StructuredStmt::CallHelper(label) => helper_call_text_cost(
            function,
            helper_params.get(label).map(Vec::as_slice).unwrap_or_default(),
        ),
        StructuredStmt::Return(values) => {
            24 + values.iter().map(|value| return_value_text_cost(function, value)).sum::<usize>()
        }
        StructuredStmt::Raise(message) => 24 + message.len(),
    }
}

fn stmt_text_cost(function: &Function, stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Assign { dst, value } => {
            local_text_cost(function, *dst) + 4 + expr_text_cost(function, value)
        }
        Stmt::Capture { key, value } => 24 + key.len() + expr_text_cost(function, value),
        Stmt::CallEffect(_) => 8,
        Stmt::Expr(value) => expr_text_cost(function, value),
        Stmt::Unsupported { dsts, text } => {
            16 + text.len() + dsts.iter().map(|dst| local_text_cost(function, *dst)).sum::<usize>()
        }
    }
}

fn return_value_text_cost(function: &Function, value: &ReturnValue) -> usize {
    match value {
        ReturnValue::Named { key, value } => key.len() + 4 + expr_text_cost(function, value),
    }
}

fn expr_text_cost(function: &Function, expr: &Expr) -> usize {
    match expr {
        Expr::Local(local) => local_text_cost(function, *local),
        Expr::Const(ConstValue::Bool(_)) => 5,
        Expr::Const(ConstValue::Int(value)) => value.to_string().len(),
        Expr::Const(ConstValue::Real(value)) => value.to_string().len(),
        Expr::Const(ConstValue::Str(value)) => value.len() + 2,
        Expr::Const(ConstValue::None) => 4,
        Expr::Unary { arg, .. } => 8 + expr_text_cost(function, arg),
        Expr::Binary { lhs, rhs, .. } => {
            8 + expr_text_cost(function, lhs) + expr_text_cost(function, rhs)
        }
        Expr::SimparamOpt { name, default } => {
            16 + expr_text_cost(function, name) + expr_text_cost(function, default)
        }
        Expr::Call { target, args } => {
            12 + target.len()
                + args.iter().map(|arg| expr_text_cost(function, arg) + 2).sum::<usize>()
        }
        Expr::Unsupported { text, args } => {
            12 + text.len()
                + args.iter().map(|arg| expr_text_cost(function, arg) + 2).sum::<usize>()
        }
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

fn safe_helper_call_counts(
    function: &Function,
    structured: &StructuredFunction,
    helper_params: &HashMap<Label, Vec<LocalId>>,
) -> HashMap<Label, usize> {
    let mut counts = HashMap::new();

    let mut defined = function.params.iter().copied().collect::<HashSet<_>>();
    count_safe_helper_calls_in_body(&structured.body, &mut defined, helper_params, &mut counts);

    for helper in &structured.helpers {
        let mut defined = helper_params
            .get(&helper.label)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<HashSet<_>>();
        count_safe_helper_calls_in_body(&helper.body, &mut defined, helper_params, &mut counts);
    }

    counts
}

fn count_safe_helper_calls_in_body(
    body: &[StructuredStmt],
    defined: &mut HashSet<LocalId>,
    helper_params: &HashMap<Label, Vec<LocalId>>,
    counts: &mut HashMap<Label, usize>,
) {
    for stmt in body {
        match stmt {
            StructuredStmt::Stmt(stmt) => mark_stmt_defs_for_definedness(stmt, defined),
            StructuredStmt::If { then_body, else_body, .. } => {
                let mut then_defined = defined.clone();
                count_safe_helper_calls_in_body(
                    then_body,
                    &mut then_defined,
                    helper_params,
                    counts,
                );
                let mut else_defined = defined.clone();
                count_safe_helper_calls_in_body(
                    else_body,
                    &mut else_defined,
                    helper_params,
                    counts,
                );
                then_defined.retain(|local| else_defined.contains(local));
                *defined = then_defined;
            }
            StructuredStmt::CallHelper(label) => {
                let safe = helper_params
                    .get(label)
                    .into_iter()
                    .flatten()
                    .all(|param| defined.contains(param));
                if safe {
                    *counts.entry(*label).or_insert(0) += 1;
                }
            }
            StructuredStmt::Return(_) | StructuredStmt::Raise(_) => {}
        }
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

fn structured_helper_call_count(body: &[StructuredStmt]) -> usize {
    body.iter().map(structured_stmt_helper_call_count).sum()
}

fn linear_helper_body(body: &[StructuredStmt]) -> bool {
    structured_helper_call_count(body) <= 1 && structured_branch_count(body) <= 1
}

fn structured_stmt_helper_call_count(stmt: &StructuredStmt) -> usize {
    match stmt {
        StructuredStmt::CallHelper(_) => 1,
        StructuredStmt::If { then_body, else_body, .. } => {
            structured_helper_call_count(then_body) + structured_helper_call_count(else_body)
        }
        StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => 0,
    }
}

fn structured_branch_count(body: &[StructuredStmt]) -> usize {
    body.iter().map(structured_stmt_branch_count).sum()
}

fn structured_stmt_branch_count(stmt: &StructuredStmt) -> usize {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            1 + structured_branch_count(then_body) + structured_branch_count(else_body)
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => 0,
    }
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
        recompute_helper_live_ins(cx)
    }
}

#[derive(Default)]
struct HelperSignaturePruning;

impl BackwardLirPass for HelperSignaturePruning {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = retain_reachable_helpers(cx);
        changed |= recompute_helper_live_ins(cx);
        changed |= prune_helper_facts(cx);
        changed
    }
}

fn retain_reachable_helpers(cx: &mut BackwardPassCx<'_>) -> bool {
    let reachable = reachable_helper_labels(cx.structured);
    let before = cx.structured.helpers.len();
    cx.structured.helpers.retain(|helper| reachable.contains(&helper.label));
    before != cx.structured.helpers.len()
}

fn reachable_helper_labels(structured: &StructuredFunction) -> HashSet<Label> {
    let helper_bodies = structured
        .helpers
        .iter()
        .map(|helper| (helper.label, helper.body.as_slice()))
        .collect::<HashMap<_, _>>();
    let mut reachable = HashSet::new();
    let mut stack = Vec::new();
    collect_helper_call_labels(&structured.body, &mut stack);

    while let Some(label) = stack.pop() {
        if !reachable.insert(label) {
            continue;
        }
        if let Some(body) = helper_bodies.get(&label) {
            collect_helper_call_labels(body, &mut stack);
        }
    }

    reachable
}

fn collect_helper_call_labels(body: &[StructuredStmt], labels: &mut Vec<Label>) {
    for stmt in body {
        match stmt {
            StructuredStmt::CallHelper(label) => labels.push(*label),
            StructuredStmt::If { then_body, else_body, .. } => {
                collect_helper_call_labels(then_body, labels);
                collect_helper_call_labels(else_body, labels);
            }
            StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => {}
        }
    }
}

fn recompute_helper_live_ins(cx: &mut BackwardPassCx<'_>) -> bool {
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

fn prune_helper_facts(cx: &mut BackwardPassCx<'_>) -> bool {
    let labels = cx.structured.helpers.iter().map(|helper| helper.label).collect::<HashSet<_>>();
    let mut changed = false;

    let before = cx.facts.helper_live_ins.len();
    cx.facts.helper_live_ins.retain(|label, _| labels.contains(label));
    changed |= cx.facts.helper_live_ins.len() != before;

    let before = cx.facts.optional_helper_live_ins.len();
    cx.facts.optional_helper_live_ins.retain(|(label, local)| {
        cx.facts.helper_live_ins.get(label).is_some_and(|params| params.contains(local))
    });
    changed |= cx.facts.optional_helper_live_ins.len() != before;

    changed
}

#[derive(Default)]
struct OptionalHelperLiveIns;

impl BackwardLirPass for OptionalHelperLiveIns {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut optional = HashSet::new();

        for helper in &cx.structured.helpers {
            let mut defined = helper.params.iter().copied().collect::<HashSet<_>>();
            collect_optional_helper_live_ins(
                &helper.body,
                &mut defined,
                &cx.facts.helper_live_ins,
                &mut optional,
            );
        }

        let mut defined = cx.function.params.iter().copied().collect::<HashSet<_>>();
        collect_optional_helper_live_ins(
            &cx.structured.body,
            &mut defined,
            &cx.facts.helper_live_ins,
            &mut optional,
        );

        let changed = cx.facts.optional_helper_live_ins != optional;
        cx.facts.optional_helper_live_ins = optional;
        changed
    }
}

fn collect_optional_helper_live_ins(
    body: &[StructuredStmt],
    defined: &mut HashSet<LocalId>,
    helper_params: &HashMap<Label, Vec<LocalId>>,
    optional: &mut HashSet<(Label, LocalId)>,
) {
    for stmt in body {
        match stmt {
            StructuredStmt::Stmt(stmt) => mark_stmt_defs_for_definedness(stmt, defined),
            StructuredStmt::If { then_body, else_body, .. } => {
                let mut then_defined = defined.clone();
                collect_optional_helper_live_ins(
                    then_body,
                    &mut then_defined,
                    helper_params,
                    optional,
                );
                let mut else_defined = defined.clone();
                collect_optional_helper_live_ins(
                    else_body,
                    &mut else_defined,
                    helper_params,
                    optional,
                );
                then_defined.retain(|local| else_defined.contains(local));
                *defined = then_defined;
            }
            StructuredStmt::CallHelper(label) => {
                for param in helper_params.get(label).into_iter().flatten() {
                    if !defined.contains(param) {
                        optional.insert((*label, *param));
                    }
                }
            }
            StructuredStmt::Return(_) | StructuredStmt::Raise(_) => {}
        }
    }
}

fn mark_stmt_defs_for_definedness(stmt: &Stmt, defined: &mut HashSet<LocalId>) {
    match stmt {
        Stmt::Assign { dst, .. } => {
            defined.insert(*dst);
        }
        Stmt::Unsupported { dsts, .. } => {
            defined.extend(dsts.iter().copied());
        }
        Stmt::Capture { .. } | Stmt::CallEffect(_) | Stmt::Expr(_) => {}
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
        StructuredStmt::Stmt(Stmt::Capture { value, .. }) => collect_expr_locals(value, live),
        StructuredStmt::Stmt(Stmt::CallEffect(effect)) => collect_call_effect_locals(effect, live),
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
        Expr::SimparamOpt { name, default } => {
            expr_has_side_effects(name) || expr_has_side_effects(default)
        }
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
        StructuredStmt::Stmt(Stmt::Capture { value, .. }) => collect_expr_locals(value, &mut live),
        StructuredStmt::Stmt(Stmt::CallEffect(effect)) => {
            collect_call_effect_locals(effect, &mut live)
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
            StructuredStmt::Stmt(Stmt::Capture { value, .. }) => {
                collect_expr_locals(value, &mut live)
            }
            StructuredStmt::Stmt(Stmt::CallEffect(effect)) => {
                collect_call_effect_locals(effect, &mut live)
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
        Expr::SimparamOpt { name, default } => {
            collect_expr_locals(name, locals);
            collect_expr_locals(default, locals);
        }
        Expr::Call { args, .. } | Expr::Unsupported { args, .. } => {
            for arg in args {
                collect_expr_locals(arg, locals);
            }
        }
    }
}

fn collect_call_effect_locals(effect: &CallEffect, locals: &mut LiveSet) {
    match effect {
        CallEffect::Diagnostic { args, .. } => {
            for arg in args {
                collect_expr_locals(arg, locals);
            }
        }
        CallEffect::SetInvalidParam { .. } | CallEffect::CollapseHint { .. } => {}
    }
}

fn collect_structured_stmt_uses(stmt: &StructuredStmt, locals: &mut Vec<LocalId>) {
    match stmt {
        StructuredStmt::Stmt(Stmt::Assign { value, .. })
        | StructuredStmt::Stmt(Stmt::Capture { value, .. })
        | StructuredStmt::Stmt(Stmt::Expr(value)) => {
            collect_expr_local_ids(value, locals);
        }
        StructuredStmt::Stmt(Stmt::CallEffect(effect)) => {
            collect_call_effect_local_ids(effect, locals);
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

fn collect_call_effect_local_ids(effect: &CallEffect, locals: &mut Vec<LocalId>) {
    match effect {
        CallEffect::Diagnostic { args, .. } => {
            for arg in args {
                collect_expr_local_ids(arg, locals);
            }
        }
        CallEffect::SetInvalidParam { .. } | CallEffect::CollapseHint { .. } => {}
    }
}

fn run_metric_guarded_post_dce_cleanup(
    function: &Function,
    structured: &mut StructuredFunction,
    facts: &mut BackwardFacts,
) -> bool {
    let before_structured = structured.clone();
    let before_facts = facts.clone();
    let before = CleanupMetrics::measure(function, structured, facts);

    let changed =
        BackwardPipeline { name: "lir-backward-post-dce-cleanup", passes: POST_DCE_CLEANUP_PASSES }
            .run(&mut BackwardPassCx { function, structured, facts: &mut *facts });
    if !changed {
        return false;
    }

    let after = CleanupMetrics::measure(function, structured, facts);
    if after.is_no_worse_than(before) {
        log_cleanup_metrics(function, "post-dce-cleanup", before, after, true);
        true
    } else {
        *structured = before_structured;
        *facts = before_facts;
        log_cleanup_metrics(function, "post-dce-cleanup", before, after, false);
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CleanupMetrics {
    helpers: usize,
    calls: usize,
    params: usize,
    cost: usize,
}

impl CleanupMetrics {
    fn measure(
        function: &Function,
        structured: &StructuredFunction,
        facts: &BackwardFacts,
    ) -> Self {
        let calls_by_helper = helper_call_counts(structured);
        let calls = calls_by_helper.values().sum::<usize>();
        let params = structured
            .helpers
            .iter()
            .map(|helper| facts.helper_live_ins.get(&helper.label).map_or(0, Vec::len))
            .sum::<usize>();
        let helper_params = structured
            .helpers
            .iter()
            .map(|helper| {
                (
                    helper.label,
                    facts.helper_live_ins.get(&helper.label).cloned().unwrap_or_default(),
                )
            })
            .collect::<HashMap<_, _>>();
        let body_cost = structured_body_text_cost(function, &structured.body, &helper_params)
            + structured
                .helpers
                .iter()
                .map(|helper| structured_body_text_cost(function, &helper.body, &helper_params))
                .sum::<usize>();
        let boundary_cost = structured
            .helpers
            .iter()
            .map(|helper| {
                helper_boundary_cost(
                    function,
                    facts.helper_live_ins.get(&helper.label).map(Vec::as_slice).unwrap_or_default(),
                    calls_by_helper.get(&helper.label).copied().unwrap_or(0),
                )
            })
            .sum::<usize>();

        Self { helpers: structured.helpers.len(), calls, params, cost: body_cost + boundary_cost }
    }

    fn is_no_worse_than(self, before: Self) -> bool {
        (self.cost, self.params, self.calls, self.helpers)
            <= (before.cost, before.params, before.calls, before.helpers)
    }
}

fn log_cleanup_metrics(
    function: &Function,
    stage: &str,
    before: CleanupMetrics,
    after: CleanupMetrics,
    kept: bool,
) {
    if std::env::var_os("MIR_LIFT_TIMING").is_none() {
        return;
    }
    eprintln!(
        "mir-lift timing: {} {} kept={} helpers {}->{} calls {}->{} params {}->{} cost {}->{}",
        function.name,
        stage,
        kept,
        before.helpers,
        after.helpers,
        before.calls,
        after.calls,
        before.params,
        after.params,
        before.cost,
        after.cost
    );
}

fn log_helper_stats(function: &Function, structured: &StructuredFunction, facts: &BackwardFacts) {
    if std::env::var_os("MIR_LIFT_TIMING").is_none() {
        return;
    }

    let mut param_counts = structured
        .helpers
        .iter()
        .map(|helper| facts.helper_live_ins.get(&helper.label).map_or(0, Vec::len))
        .collect::<Vec<_>>();
    param_counts.sort_unstable();

    let helper_count = param_counts.len();
    let param_total = param_counts.iter().sum::<usize>();
    let param_mean = if helper_count == 0 { 0 } else { param_total / helper_count };
    let param_p90 = percentile(&param_counts, 90);
    let param_max = param_counts.last().copied().unwrap_or(0);
    let call_count = helper_call_counts(structured).values().sum::<usize>();
    let boundary_cost = structured
        .helpers
        .iter()
        .map(|helper| {
            helper_boundary_cost(
                function,
                facts.helper_live_ins.get(&helper.label).map(Vec::as_slice).unwrap_or_default(),
                helper_call_counts(structured).get(&helper.label).copied().unwrap_or(0),
            )
        })
        .sum::<usize>();

    eprintln!(
        "mir-lift timing: {} lir-helper-stats helpers={} calls={} params_mean={} params_p90={} params_max={} boundary_cost={}",
        function.name,
        helper_count,
        call_count,
        param_mean,
        param_p90,
        param_max,
        boundary_cost
    );
}

fn percentile(sorted: &[usize], pct: usize) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) * pct) / 100;
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::{BinaryOp, ConstValue, Function, LirType, Local, ReturnValue};

    #[test]
    fn sinks_common_tail_helper_after_branch_assignments() {
        let target = Label(7);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 0), StructuredStmt::CallHelper(target)],
            else_body: vec![assign_int(LocalId(1), 1), StructuredStmt::CallHelper(target)],
        }];

        assert!(sink_common_tail_helpers_in_body(&mut body));
        assert_eq!(
            body,
            vec![
                StructuredStmt::If {
                    cond: Expr::Local(LocalId(0)),
                    then_body: vec![assign_int(LocalId(1), 0)],
                    else_body: vec![assign_int(LocalId(1), 1)],
                },
                StructuredStmt::CallHelper(target),
            ]
        );
    }

    #[test]
    fn sinks_common_tail_helper_from_empty_branches() {
        let target = Label(11);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Binary {
                op: BinaryOp::Eq,
                lhs: Box::new(Expr::Local(LocalId(0))),
                rhs: Box::new(Expr::Const(ConstValue::Int(0))),
            },
            then_body: vec![StructuredStmt::CallHelper(target)],
            else_body: vec![StructuredStmt::CallHelper(target)],
        }];

        assert!(sink_common_tail_helpers_in_body(&mut body));
        assert_eq!(
            body,
            vec![
                StructuredStmt::If {
                    cond: Expr::Binary {
                        op: BinaryOp::Eq,
                        lhs: Box::new(Expr::Local(LocalId(0))),
                        rhs: Box::new(Expr::Const(ConstValue::Int(0))),
                    },
                    then_body: Vec::new(),
                    else_body: Vec::new(),
                },
                StructuredStmt::CallHelper(target),
            ]
        );
        assert!(simplify_body(&mut body));
        assert_eq!(body, vec![StructuredStmt::CallHelper(target)]);
    }

    #[test]
    fn does_not_create_one_sided_empty_branch() {
        let target = Label(13);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![StructuredStmt::CallHelper(target)],
            else_body: vec![assign_int(LocalId(1), 1), StructuredStmt::CallHelper(target)],
        }];

        assert!(!sink_common_tail_helpers_in_body(&mut body));
    }

    #[test]
    fn signature_pruning_drops_unreachable_helpers_and_stale_params() {
        let function = test_function();
        let live = Label(1);
        let dead = Label(2);
        let mut structured = StructuredFunction {
            body: vec![StructuredStmt::CallHelper(live)],
            helpers: vec![
                crate::lir_structure::StructuredHelper {
                    label: live,
                    params: vec![LocalId(0), LocalId(1)],
                    body: vec![
                        assign_int(LocalId(1), 1),
                        StructuredStmt::Return(vec![ReturnValue::Named {
                            key: "x".to_owned(),
                            value: Expr::Local(LocalId(1)),
                        }]),
                    ],
                },
                crate::lir_structure::StructuredHelper {
                    label: dead,
                    params: vec![LocalId(0)],
                    body: vec![StructuredStmt::Return(vec![ReturnValue::Named {
                        key: "y".to_owned(),
                        value: Expr::Local(LocalId(0)),
                    }])],
                },
            ],
            facts: BackwardFacts::default(),
        };
        let mut facts = BackwardFacts {
            helper_live_ins: HashMap::from([
                (live, vec![LocalId(0), LocalId(1)]),
                (dead, vec![LocalId(0)]),
            ]),
            optional_helper_live_ins: HashSet::from([(dead, LocalId(0))]),
        };

        let mut pass = HelperSignaturePruning;
        let mut cx =
            BackwardPassCx { function: &function, structured: &mut structured, facts: &mut facts };

        assert!(pass.run(&mut cx));
        assert_eq!(structured.helpers.len(), 1);
        assert_eq!(structured.helpers[0].label, live);
        assert!(structured.helpers[0].params.is_empty());
        assert_eq!(facts.helper_live_ins.get(&live), Some(&Vec::new()));
        assert!(!facts.helper_live_ins.contains_key(&dead));
        assert!(facts.optional_helper_live_ins.is_empty());
    }

    fn assign_int(dst: LocalId, value: i32) -> StructuredStmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value: Expr::Const(ConstValue::Int(value)) })
    }

    fn test_function() -> Function {
        Function {
            name: "test".to_owned(),
            params: vec![LocalId(0)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "p".to_owned(), ty: LirType::Int },
                Local { id: LocalId(1), name_hint: "x".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
        }
    }
}
