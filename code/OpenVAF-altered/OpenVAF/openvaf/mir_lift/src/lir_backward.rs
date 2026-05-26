use std::collections::{HashMap, HashSet};

use crate::lir::{
    BinaryOp, CallEffect, ConstValue, Expr, Function, Label, LirType, LocalId, ReturnValue, Stmt,
    UnaryOp,
};
use crate::lir_structure::{StructuredFunction, StructuredStmt};

const DEFAULT_CLEANUP_ROUNDS: usize = 12;
const DEFAULT_FINAL_DCE_ROUNDS: usize = 3;
const DEFAULT_POST_DCE_CLEANUP_ROUNDS: usize = 0;
const SMALL_HELPER_STMT_LIMIT: usize = 12;
const COST_INLINE_HELPER_CALL_LIMIT: usize = 3;
const COST_INLINE_MIN_SAVINGS: usize = 32;
const PUSH_UP_HELPER_CALL_LIMIT: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackwardFacts {
    pub entry_live_ins: Vec<LocalId>,
    pub helper_live_ins: HashMap<Label, Vec<LocalId>>,
    pub optional_helper_live_ins: HashSet<(Label, LocalId)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BackwardRoundCounts {
    cleanup: usize,
    final_dce: usize,
    post_dce_cleanup: usize,
}

impl BackwardRoundCounts {
    fn from_env() -> Self {
        Self {
            cleanup: env_round_count("MIR_LIFT_LIR_CLEANUP_ROUNDS", DEFAULT_CLEANUP_ROUNDS),
            final_dce: env_round_count("MIR_LIFT_LIR_FINAL_DCE_ROUNDS", DEFAULT_FINAL_DCE_ROUNDS),
            post_dce_cleanup: env_round_count(
                "MIR_LIFT_LIR_POST_DCE_CLEANUP_ROUNDS",
                DEFAULT_POST_DCE_CLEANUP_ROUNDS,
            ),
        }
    }
}

fn env_round_count(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|value| value.parse::<usize>().ok()).unwrap_or(default)
}

const DISABLED_PASSES: &[BackwardPassKind] = &[BackwardPassKind::HelperLiveIns];
const CLEANUP_PASSES: &[BackwardPassKind] = &[
    BackwardPassKind::DropNonSemanticEffects,
    BackwardPassKind::HelperForwarding,
    BackwardPassKind::CommonTailHelperSinking,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CostBasedHelperInlining,
    BackwardPassKind::StructuredSimplify,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::HelperComputationPushUp,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CopyAliasPropagation,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::CopyAliasPropagation,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::HelperSignaturePruning,
];
const STRUCTURAL_PASSES: &[BackwardPassKind] = &[
    BackwardPassKind::HelperForwarding,
    BackwardPassKind::CommonTailHelperSinking,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CostBasedHelperInlining,
    BackwardPassKind::StructuredSimplify,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::HelperComputationPushUp,
    BackwardPassKind::HelperSignaturePruning,
];
const FINAL_DCE_PASSES: &[BackwardPassKind] = &[
    BackwardPassKind::DropNonSemanticEffects,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CopyAliasPropagation,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::CopyAliasPropagation,
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
    let rounds = BackwardRoundCounts::from_env();

    for _ in 0..rounds.cleanup {
        if !cleanup_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts }) {
            break;
        }
    }

    structural_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts });

    for _ in 0..rounds.final_dce {
        if !final_dce_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts })
        {
            break;
        }
    }

    for _ in 0..rounds.post_dce_cleanup {
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
    DropNonSemanticEffects,
    HelperForwarding,
    CommonTailHelperSinking,
    HelperSignaturePruning,
    CostBasedHelperInlining,
    StructuredSimplify,
    BranchInvariantHoist,
    BranchSquash,
    HelperComputationPushUp,
    HelperLiveIns,
    OptionalHelperLiveIns,
    CopyAliasPropagation,
    DeadAssignments,
}

impl BackwardPassKind {
    fn name(self) -> &'static str {
        match self {
            Self::DropNonSemanticEffects => "drop-non-semantic-effects",
            Self::HelperForwarding => "helper-forwarding",
            Self::CommonTailHelperSinking => "common-tail-helper-sinking",
            Self::HelperSignaturePruning => "helper-signature-pruning",
            Self::CostBasedHelperInlining => "cost-based-helper-inlining",
            Self::StructuredSimplify => "structured-simplify",
            Self::BranchInvariantHoist => "branch-invariant-hoist",
            Self::BranchSquash => "branch-squash",
            Self::HelperComputationPushUp => "helper-computation-push-up",
            Self::HelperLiveIns => "helper-live-ins",
            Self::OptionalHelperLiveIns => "optional-helper-live-ins",
            Self::CopyAliasPropagation => "copy-alias-propagation",
            Self::DeadAssignments => "dead-assignments",
        }
    }

    fn run(self, cx: &mut BackwardPassCx<'_>) -> bool {
        match self {
            Self::DropNonSemanticEffects => run_backward_pass::<DropNonSemanticEffects>(cx),
            Self::HelperForwarding => run_backward_pass::<HelperForwarding>(cx),
            Self::CommonTailHelperSinking => run_backward_pass::<CommonTailHelperSinking>(cx),
            Self::HelperSignaturePruning => run_backward_pass::<HelperSignaturePruning>(cx),
            Self::CostBasedHelperInlining => run_backward_pass::<CostBasedHelperInlining>(cx),
            Self::StructuredSimplify => run_backward_pass::<StructuredSimplify>(cx),
            Self::BranchInvariantHoist => run_backward_pass::<BranchInvariantHoist>(cx),
            Self::BranchSquash => run_backward_pass::<BranchSquash>(cx),
            Self::HelperComputationPushUp => run_backward_pass::<HelperComputationPushUp>(cx),
            Self::HelperLiveIns => run_backward_pass::<HelperLiveIns>(cx),
            Self::OptionalHelperLiveIns => run_backward_pass::<OptionalHelperLiveIns>(cx),
            Self::CopyAliasPropagation => run_backward_pass::<CopyAliasPropagation>(cx),
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

    fn update_entry_params(&mut self, params: Vec<LocalId>) -> bool {
        let changed = self.facts.entry_live_ins != params;
        if changed {
            self.facts.entry_live_ins = params;
        }
        changed
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
struct DropNonSemanticEffects;

impl BackwardLirPass for DropNonSemanticEffects {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = drop_non_semantic_effects_in_body(&mut cx.structured.body);
        for helper in &mut cx.structured.helpers {
            changed |= drop_non_semantic_effects_in_body(&mut helper.body);
        }
        changed
    }
}

fn drop_non_semantic_effects_in_body(body: &mut Vec<StructuredStmt>) -> bool {
    let mut changed = false;
    let mut kept = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..) {
        changed |= drop_non_semantic_effects_in_stmt(&mut stmt);
        if is_non_semantic_effect_stmt(&stmt) {
            changed = true;
        } else {
            kept.push(stmt);
        }
    }

    *body = kept;
    changed
}

fn drop_non_semantic_effects_in_stmt(stmt: &mut StructuredStmt) -> bool {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            let then_changed = drop_non_semantic_effects_in_body(then_body);
            let else_changed = drop_non_semantic_effects_in_body(else_body);
            then_changed || else_changed
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    }
}

fn is_non_semantic_effect_stmt(stmt: &StructuredStmt) -> bool {
    matches!(
        stmt,
        StructuredStmt::Stmt(Stmt::CallEffect(
            CallEffect::Diagnostic { .. } | CallEffect::CollapseHint { .. }
        ))
    )
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

#[derive(Default)]
struct BranchInvariantHoist;

impl BackwardLirPass for BranchInvariantHoist {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = hoist_branch_invariants_in_body(&mut cx.structured.body);
        for helper in &mut cx.structured.helpers {
            changed |= hoist_branch_invariants_in_body(&mut helper.body);
        }
        changed
    }
}

fn hoist_branch_invariants_in_body(body: &mut Vec<StructuredStmt>) -> bool {
    let mut changed = false;
    let mut rewritten = Vec::with_capacity(body.len());

    for mut stmt in body.drain(..) {
        changed |= hoist_branch_invariants_in_stmt(&mut stmt);

        match stmt {
            StructuredStmt::If { cond, mut then_body, mut else_body } => {
                let prefix_len = common_hoistable_branch_prefix_len(&cond, &then_body, &else_body);
                if prefix_len > 0 {
                    rewritten.extend(then_body.drain(..prefix_len));
                    let _ = else_body.drain(..prefix_len).count();
                    changed = true;
                }

                let suffix_len = common_hoistable_branch_suffix_len(&then_body, &else_body);
                let suffix = if suffix_len > 0 {
                    let suffix_start = then_body.len() - suffix_len;
                    let suffix = then_body.split_off(suffix_start);
                    else_body.truncate(else_body.len() - suffix_len);
                    changed = true;
                    suffix
                } else {
                    Vec::new()
                };

                rewritten.push(StructuredStmt::If { cond, then_body, else_body });
                rewritten.extend(suffix);
            }
            other => rewritten.push(other),
        }
    }

    *body = rewritten;
    changed
}

fn hoist_branch_invariants_in_stmt(stmt: &mut StructuredStmt) -> bool {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            let then_changed = hoist_branch_invariants_in_body(then_body);
            let else_changed = hoist_branch_invariants_in_body(else_body);
            then_changed || else_changed
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    }
}

fn common_hoistable_branch_prefix_len(
    cond: &Expr,
    then_body: &[StructuredStmt],
    else_body: &[StructuredStmt],
) -> usize {
    if expr_has_side_effects(cond) {
        return 0;
    }

    let mut cond_locals = Vec::new();
    collect_expr_local_ids(cond, &mut cond_locals);

    let mut len = 0;
    while let (Some(then_stmt), Some(else_stmt)) = (then_body.get(len), else_body.get(len)) {
        if then_stmt != else_stmt {
            break;
        }
        let Some(dst) = hoistable_pure_assignment_dst(then_stmt) else {
            break;
        };
        if cond_locals.contains(&dst) {
            break;
        }
        len += 1;
    }
    len
}

fn common_hoistable_branch_suffix_len(
    then_body: &[StructuredStmt],
    else_body: &[StructuredStmt],
) -> usize {
    let mut len = 0;
    while len < then_body.len() && len < else_body.len() {
        let then_index = then_body.len() - 1 - len;
        let else_index = else_body.len() - 1 - len;
        let then_stmt = &then_body[then_index];
        let else_stmt = &else_body[else_index];
        if then_stmt != else_stmt || hoistable_pure_assignment_dst(then_stmt).is_none() {
            break;
        }
        len += 1;
    }
    len
}

fn hoistable_pure_assignment_dst(stmt: &StructuredStmt) -> Option<LocalId> {
    let StructuredStmt::Stmt(Stmt::Assign { dst, value }) = stmt else {
        return None;
    };
    (!expr_has_side_effects(value)).then_some(*dst)
}

#[derive(Default)]
struct BranchSquash;

impl BackwardLirPass for BranchSquash {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let local_types = local_types(cx.function);
        let mut changed = squash_branches_in_body(&mut cx.structured.body, &local_types);
        for helper in &mut cx.structured.helpers {
            changed |= squash_branches_in_body(&mut helper.body, &local_types);
        }
        changed
    }
}

fn squash_branches_in_body(body: &mut [StructuredStmt], local_types: &[LirType]) -> bool {
    let mut changed = false;
    for stmt in body {
        changed |= squash_branches_in_stmt(stmt, local_types);
    }
    changed
}

fn squash_branches_in_stmt(stmt: &mut StructuredStmt, local_types: &[LirType]) -> bool {
    let mut changed = match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            squash_branches_in_body(then_body, local_types)
                | squash_branches_in_body(else_body, local_types)
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    };

    if let Some(replacement) = squashed_branch_assignment(stmt, local_types) {
        *stmt = replacement;
        changed = true;
    }

    changed
}

fn squashed_branch_assignment(
    stmt: &StructuredStmt,
    local_types: &[LirType],
) -> Option<StructuredStmt> {
    let StructuredStmt::If { cond, then_body, else_body } = stmt else {
        return None;
    };
    if expr_has_side_effects(cond) || expr_type(cond, local_types) != Some(LirType::Bool) {
        return None;
    }

    let (dst, then_value) = single_assignment(then_body)?;
    let (else_dst, else_value) = single_assignment(else_body)?;
    if dst != else_dst {
        return None;
    }

    let dst_ty = *local_types.get(dst.0)?;
    let value = squashed_boolish_branch_expr(cond, then_value, else_value, dst_ty)
        .or_else(|| squashed_int_const_branch_expr(cond, then_value, else_value, dst_ty))?;

    Some(StructuredStmt::Stmt(Stmt::Assign { dst, value }))
}

fn squashed_boolish_branch_expr(
    cond: &Expr,
    then_value: &ConstValue,
    else_value: &ConstValue,
    dst_ty: LirType,
) -> Option<Expr> {
    let (kind, then_truthy, else_truthy) = boolish_const_pair(then_value, else_value)?;
    let kind = replacement_kind_for_dst(kind, dst_ty)?;
    match (kind, then_truthy, else_truthy) {
        (BoolishKind::Bool, true, false) => Some(cond.clone()),
        (BoolishKind::Bool, false, true) => {
            Some(Expr::Unary { op: UnaryOp::Not, arg: Box::new(cond.clone()) })
        }
        (BoolishKind::Int, true, false) => {
            Some(Expr::Unary { op: UnaryOp::Cast(LirType::Int), arg: Box::new(cond.clone()) })
        }
        (BoolishKind::Int, false, true) => Some(Expr::Unary {
            op: UnaryOp::Cast(LirType::Int),
            arg: Box::new(Expr::Unary { op: UnaryOp::Not, arg: Box::new(cond.clone()) }),
        }),
        _ => None,
    }
}

fn squashed_int_const_branch_expr(
    cond: &Expr,
    then_value: &ConstValue,
    else_value: &ConstValue,
    dst_ty: LirType,
) -> Option<Expr> {
    if dst_ty != LirType::Int {
        return None;
    }
    let (ConstValue::Int(then_int), ConstValue::Int(else_int)) = (then_value, else_value) else {
        return None;
    };
    let delta = checked_int_branch_delta(*then_int, *else_int)?;

    Some(Expr::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(Expr::Const(ConstValue::Int(*else_int))),
        rhs: Box::new(Expr::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(Expr::Unary {
                op: UnaryOp::Cast(LirType::Int),
                arg: Box::new(cond.clone()),
            }),
            rhs: Box::new(Expr::Const(ConstValue::Int(delta))),
        }),
    })
}

fn checked_int_branch_delta(then_int: i32, else_int: i32) -> Option<i32> {
    let delta = then_int.checked_sub(else_int)?;
    for cond_value in [0, 1] {
        let product = delta.checked_mul(cond_value)?;
        let result = else_int.checked_add(product)?;
        let expected = if cond_value == 0 { else_int } else { then_int };
        if result != expected {
            return None;
        }
    }
    Some(delta)
}

fn single_assignment(body: &[StructuredStmt]) -> Option<(LocalId, &ConstValue)> {
    let [StructuredStmt::Stmt(Stmt::Assign { dst, value: Expr::Const(value) })] = body else {
        return None;
    };
    Some((*dst, value))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoolishKind {
    Bool,
    Int,
}

fn boolish_const_pair(
    then_value: &ConstValue,
    else_value: &ConstValue,
) -> Option<(BoolishKind, bool, bool)> {
    let (then_kind, then_truthy) = boolish_const(then_value)?;
    let (else_kind, else_truthy) = boolish_const(else_value)?;
    (then_kind == else_kind).then_some((then_kind, then_truthy, else_truthy))
}

fn boolish_const(value: &ConstValue) -> Option<(BoolishKind, bool)> {
    match value {
        ConstValue::Bool(value) => Some((BoolishKind::Bool, *value)),
        ConstValue::Int(0) => Some((BoolishKind::Int, false)),
        ConstValue::Int(1) => Some((BoolishKind::Int, true)),
        _ => None,
    }
}

fn replacement_kind_for_dst(kind: BoolishKind, dst_ty: LirType) -> Option<BoolishKind> {
    match (dst_ty, kind) {
        (LirType::Bool, BoolishKind::Bool)
        | (LirType::Int, BoolishKind::Int)
        | (LirType::Unknown, _) => Some(kind),
        _ => None,
    }
}

fn local_types(function: &Function) -> Vec<LirType> {
    function.locals.iter().map(|local| local.ty).collect()
}

fn expr_type(expr: &Expr, local_types: &[LirType]) -> Option<LirType> {
    match expr {
        Expr::Local(local) => local_types.get(local.0).copied(),
        Expr::Const(ConstValue::Bool(_)) => Some(LirType::Bool),
        Expr::Const(ConstValue::Int(_)) => Some(LirType::Int),
        Expr::Const(ConstValue::Real(_)) => Some(LirType::Real),
        Expr::Const(ConstValue::Str(_)) => Some(LirType::Str),
        Expr::Const(ConstValue::None) => None,
        Expr::Unary { op: UnaryOp::Not, .. } => Some(LirType::Bool),
        Expr::Unary { op: UnaryOp::Neg, arg } => expr_type(arg, local_types),
        Expr::Unary { op: UnaryOp::Cast(ty), .. } => Some(*ty),
        Expr::Unary { op: UnaryOp::Math1(_), .. } => Some(LirType::Real),
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
            | BinaryOp::Ge => Some(LirType::Bool),
            BinaryOp::Math2(_) => Some(LirType::Real),
        },
        Expr::SimparamOpt { default, .. } => expr_type(default, local_types),
        Expr::Call { .. } | Expr::Unsupported { .. } => None,
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
        let mut changed = recompute_helper_live_ins(cx);
        changed |= recompute_entry_live_ins(cx);
        changed
    }
}

#[derive(Default)]
struct HelperSignaturePruning;

impl BackwardLirPass for HelperSignaturePruning {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = retain_reachable_helpers(cx);
        changed |= recompute_helper_live_ins(cx);
        changed |= recompute_entry_live_ins(cx);
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

fn recompute_entry_live_ins(cx: &mut BackwardPassCx<'_>) -> bool {
    let bodies = cx
        .structured
        .helpers
        .iter()
        .map(|helper| (helper.label, helper.body.clone()))
        .collect::<HashMap<_, _>>();
    let mut computed = HashMap::new();
    let mut visiting = Vec::new();
    let raw_params = cx.function.params.iter().copied().collect::<HashSet<_>>();
    let params = live_in_body_static(
        &cx.structured.body,
        LiveSet::new(cx.function.locals.len()),
        cx.function.locals.len(),
        &bodies,
        &mut computed,
        &mut visiting,
    )
    .into_sorted_locals()
    .into_iter()
    .filter(|local| raw_params.contains(local))
    .collect::<Vec<_>>();

    cx.update_entry_params(params)
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
        Stmt::Assign { dst, value } => {
            if expr_is_definitely_defined_by(value, defined) {
                defined.insert(*dst);
            } else {
                defined.remove(dst);
            }
        }
        Stmt::Unsupported { dsts, .. } => {
            defined.extend(dsts.iter().copied());
        }
        Stmt::Capture { .. } | Stmt::CallEffect(_) | Stmt::Expr(_) => {}
    }
}

fn expr_is_definitely_defined_by(expr: &Expr, defined: &HashSet<LocalId>) -> bool {
    let mut locals = Vec::new();
    collect_expr_local_ids(expr, &mut locals);
    locals.into_iter().all(|local| defined.contains(&local))
}

#[derive(Default)]
struct CopyAliasPropagation;

impl BackwardLirPass for CopyAliasPropagation {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let mut changed = false;

        for helper in &mut cx.structured.helpers {
            let mut aliases = HashMap::new();
            changed |= propagate_copy_aliases_in_body(&mut helper.body, &mut aliases);
        }

        let mut aliases = HashMap::new();
        changed |= propagate_copy_aliases_in_body(&mut cx.structured.body, &mut aliases);
        changed
    }
}

type CopyAliasMap = HashMap<LocalId, LocalId>;

fn propagate_copy_aliases_in_body(body: &mut [StructuredStmt], aliases: &mut CopyAliasMap) -> bool {
    let mut changed = false;
    for stmt in body {
        changed |= propagate_copy_aliases_in_stmt(stmt, aliases);
    }
    changed
}

fn propagate_copy_aliases_in_stmt(stmt: &mut StructuredStmt, aliases: &mut CopyAliasMap) -> bool {
    match stmt {
        StructuredStmt::Stmt(stmt) => propagate_copy_aliases_in_plain_stmt(stmt, aliases),
        StructuredStmt::If { cond, then_body, else_body } => {
            let mut changed = rewrite_alias_expr(cond, aliases);

            let incoming = aliases.clone();
            let mut then_aliases = incoming.clone();
            let mut else_aliases = incoming;
            changed |= propagate_copy_aliases_in_body(then_body, &mut then_aliases);
            changed |= propagate_copy_aliases_in_body(else_body, &mut else_aliases);
            *aliases = intersect_alias_maps(&then_aliases, &else_aliases);
            changed
        }
        StructuredStmt::CallHelper(_) => {
            aliases.clear();
            false
        }
        StructuredStmt::Return(values) => {
            let mut changed = false;
            for value in values {
                match value {
                    ReturnValue::Named { value, .. } => {
                        changed |= rewrite_alias_expr(value, aliases);
                    }
                }
            }
            aliases.clear();
            changed
        }
        StructuredStmt::Raise(_) => {
            aliases.clear();
            false
        }
    }
}

fn propagate_copy_aliases_in_plain_stmt(stmt: &mut Stmt, aliases: &mut CopyAliasMap) -> bool {
    match stmt {
        Stmt::Assign { dst, value } => {
            let changed = rewrite_alias_expr(value, aliases);
            let copied = match value {
                Expr::Local(src) => Some(canonical_alias(*src, aliases)),
                _ => None,
            };
            kill_aliases_for_definition(aliases, *dst);
            if let Some(src) = copied.filter(|src| *src != *dst) {
                aliases.insert(*dst, src);
            }
            changed
        }
        Stmt::Capture { value, .. } | Stmt::Expr(value) => rewrite_alias_expr(value, aliases),
        Stmt::CallEffect(effect) => rewrite_alias_call_effect(effect, aliases),
        Stmt::Unsupported { dsts, .. } => {
            for dst in dsts {
                kill_aliases_for_definition(aliases, *dst);
            }
            false
        }
    }
}

fn rewrite_alias_expr(expr: &mut Expr, aliases: &CopyAliasMap) -> bool {
    match expr {
        Expr::Local(local) => {
            let canonical = canonical_alias(*local, aliases);
            let changed = canonical != *local;
            *local = canonical;
            changed
        }
        Expr::Const(_) => false,
        Expr::Unary { arg, .. } => rewrite_alias_expr(arg, aliases),
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_alias_expr(lhs, aliases) | rewrite_alias_expr(rhs, aliases)
        }
        Expr::SimparamOpt { name, default } => {
            rewrite_alias_expr(name, aliases) | rewrite_alias_expr(default, aliases)
        }
        Expr::Call { args, .. } | Expr::Unsupported { args, .. } => {
            let mut changed = false;
            for arg in args {
                changed |= rewrite_alias_expr(arg, aliases);
            }
            changed
        }
    }
}

fn rewrite_alias_call_effect(effect: &mut CallEffect, aliases: &CopyAliasMap) -> bool {
    match effect {
        CallEffect::Diagnostic { args, .. } => {
            let mut changed = false;
            for arg in args {
                changed |= rewrite_alias_expr(arg, aliases);
            }
            changed
        }
        CallEffect::SetInvalidParam { .. } | CallEffect::CollapseHint { .. } => false,
    }
}

fn canonical_alias(local: LocalId, aliases: &CopyAliasMap) -> LocalId {
    let mut current = local;
    let mut seen = HashSet::new();
    while let Some(&next) = aliases.get(&current) {
        if next == current || !seen.insert(current) {
            break;
        }
        current = next;
    }
    current
}

fn kill_aliases_for_definition(aliases: &mut CopyAliasMap, dst: LocalId) {
    aliases.remove(&dst);
    aliases.retain(|alias, target| *alias != dst && *target != dst);
}

fn intersect_alias_maps(left: &CopyAliasMap, right: &CopyAliasMap) -> CopyAliasMap {
    left.iter()
        .filter_map(|(alias, target)| {
            right
                .get(alias)
                .and_then(|right_target| (right_target == target).then_some((*alias, *target)))
        })
        .collect()
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
    use crate::lir::{BinaryOp, CallEffect, ConstValue, Function, LirType, Local, ReturnValue};

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
            entry_live_ins: Vec::new(),
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

    #[test]
    fn signature_pruning_tracks_entry_live_ins_for_raw_params_only() {
        let function = Function {
            name: "test".to_owned(),
            params: vec![LocalId(0), LocalId(1)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "unused".to_owned(), ty: LirType::Int },
                Local { id: LocalId(1), name_hint: "used".to_owned(), ty: LirType::Int },
                Local { id: LocalId(2), name_hint: "tmp".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
            output_types: HashMap::new(),
        };
        let mut structured = StructuredFunction {
            body: vec![
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(2),
                    value: Expr::Local(LocalId(1)),
                }),
                StructuredStmt::Return(vec![ReturnValue::Named {
                    key: "x".to_owned(),
                    value: Expr::Local(LocalId(2)),
                }]),
            ],
            helpers: Vec::new(),
            facts: BackwardFacts::default(),
        };
        let mut facts = BackwardFacts::default();

        let mut pass = HelperSignaturePruning;
        let mut cx =
            BackwardPassCx { function: &function, structured: &mut structured, facts: &mut facts };

        assert!(pass.run(&mut cx));
        assert_eq!(facts.entry_live_ins, vec![LocalId(1)]);
    }

    #[test]
    fn drops_only_non_semantic_effects() {
        let mut body = vec![
            StructuredStmt::Stmt(Stmt::CallEffect(CallEffect::Diagnostic {
                target: "Display".to_owned(),
                args: vec![Expr::Local(LocalId(0))],
            })),
            StructuredStmt::Stmt(Stmt::CallEffect(CallEffect::SetInvalidParam {
                param: "p1".to_owned(),
            })),
            StructuredStmt::If {
                cond: Expr::Local(LocalId(0)),
                then_body: vec![
                    StructuredStmt::Stmt(Stmt::CallEffect(CallEffect::Diagnostic {
                        target: "Warning".to_owned(),
                        args: vec![Expr::Local(LocalId(1))],
                    })),
                    assign_int(LocalId(1), 2),
                ],
                else_body: vec![StructuredStmt::Stmt(Stmt::CallEffect(CallEffect::CollapseHint {
                    hi: "n2".to_owned(),
                    lo: Some("n1".to_owned()),
                }))],
            },
        ];

        assert!(drop_non_semantic_effects_in_body(&mut body));
        assert_eq!(
            body,
            vec![
                StructuredStmt::Stmt(Stmt::CallEffect(CallEffect::SetInvalidParam {
                    param: "p1".to_owned(),
                })),
                StructuredStmt::If {
                    cond: Expr::Local(LocalId(0)),
                    then_body: vec![assign_int(LocalId(1), 2)],
                    else_body: Vec::new(),
                },
            ]
        );
        assert!(!drop_non_semantic_effects_in_body(&mut body));
    }

    #[test]
    fn branch_invariant_hoist_moves_common_prefix_assignments() {
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![
                assign_int(LocalId(2), 7),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Local(LocalId(2)),
                }),
                assign_int(LocalId(1), 1),
            ],
            else_body: vec![
                assign_int(LocalId(2), 7),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Local(LocalId(2)),
                }),
                assign_int(LocalId(1), 0),
            ],
        }];

        assert!(hoist_branch_invariants_in_body(&mut body));
        assert_eq!(
            body,
            vec![
                assign_int(LocalId(2), 7),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Local(LocalId(2)),
                }),
                StructuredStmt::If {
                    cond: Expr::Local(LocalId(0)),
                    then_body: vec![assign_int(LocalId(1), 1)],
                    else_body: vec![assign_int(LocalId(1), 0)],
                },
            ]
        );
    }

    #[test]
    fn branch_invariant_hoist_moves_common_suffix_assignments() {
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![
                assign_int(LocalId(1), 1),
                assign_int(LocalId(2), 7),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Local(LocalId(2)),
                }),
            ],
            else_body: vec![
                assign_int(LocalId(1), 0),
                assign_int(LocalId(2), 7),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Local(LocalId(2)),
                }),
            ],
        }];

        assert!(hoist_branch_invariants_in_body(&mut body));
        assert_eq!(
            body,
            vec![
                StructuredStmt::If {
                    cond: Expr::Local(LocalId(0)),
                    then_body: vec![assign_int(LocalId(1), 1)],
                    else_body: vec![assign_int(LocalId(1), 0)],
                },
                assign_int(LocalId(2), 7),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Local(LocalId(2)),
                }),
            ]
        );
    }

    #[test]
    fn branch_invariant_hoist_rejects_effecting_and_non_assignment_prefixes() {
        let target = Label(19);
        let original = vec![
            StructuredStmt::If {
                cond: Expr::Local(LocalId(0)),
                then_body: vec![StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Call { target: "probe".to_owned(), args: Vec::new() },
                })],
                else_body: vec![StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Call { target: "probe".to_owned(), args: Vec::new() },
                })],
            },
            StructuredStmt::If {
                cond: Expr::Local(LocalId(0)),
                then_body: vec![StructuredStmt::Stmt(Stmt::Capture {
                    key: "x".to_owned(),
                    value: Expr::Local(LocalId(1)),
                })],
                else_body: vec![StructuredStmt::Stmt(Stmt::Capture {
                    key: "x".to_owned(),
                    value: Expr::Local(LocalId(1)),
                })],
            },
            StructuredStmt::If {
                cond: Expr::Local(LocalId(0)),
                then_body: vec![StructuredStmt::CallHelper(target)],
                else_body: vec![StructuredStmt::CallHelper(target)],
            },
        ];
        let mut body = original.clone();

        assert!(!hoist_branch_invariants_in_body(&mut body));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_invariant_hoist_rejects_prefix_assignment_used_by_condition() {
        let original = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(2)),
            then_body: vec![assign_int(LocalId(2), 7), assign_int(LocalId(1), 1)],
            else_body: vec![assign_int(LocalId(2), 7), assign_int(LocalId(1), 0)],
        }];
        let mut body = original.clone();

        assert!(!hoist_branch_invariants_in_body(&mut body));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_invariant_hoist_exposes_branch_squash() {
        let function = bool_condition_function(LirType::Int);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(2), 7), assign_int(LocalId(1), 1)],
            else_body: vec![assign_int(LocalId(2), 7), assign_int(LocalId(1), 0)],
        }];

        assert!(hoist_branch_invariants_in_body(&mut body));
        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                assign_int(LocalId(2), 7),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Unary {
                        op: UnaryOp::Cast(LirType::Int),
                        arg: Box::new(Expr::Local(LocalId(0))),
                    },
                }),
            ]
        );
    }

    #[test]
    fn branch_squash_replaces_direct_int_branch_with_cast_condition() {
        let function = bool_condition_function(LirType::Int);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 1)],
            else_body: vec![assign_int(LocalId(1), 0)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Unary {
                    op: UnaryOp::Cast(LirType::Int),
                    arg: Box::new(Expr::Local(LocalId(0))),
                },
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_direct_int_constant_branch_with_arithmetic_condition() {
        let function = bool_condition_function(LirType::Int);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 3)],
            else_body: vec![assign_int(LocalId(1), 5)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: int_branch_arithmetic_expr(5, -2),
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_negative_int_constant_branch_with_arithmetic_condition() {
        let function = bool_condition_function(LirType::Int);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), -7)],
            else_body: vec![assign_int(LocalId(1), -2)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: int_branch_arithmetic_expr(-2, -5),
            })]
        );
    }

    #[test]
    fn branch_squash_rejects_overflowing_int_constant_branch() {
        let function = bool_condition_function(LirType::Int);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), i32::MIN)],
            else_body: vec![assign_int(LocalId(1), i32::MAX)],
        }];

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
    }

    #[test]
    fn branch_squash_rejects_real_mixed_and_unknown_int_constant_branches() {
        let real_function = bool_condition_function(LirType::Real);
        let mut real_body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_real(LocalId(1), 3.0)],
            else_body: vec![assign_real(LocalId(1), 5.0)],
        }];
        let int_function = bool_condition_function(LirType::Int);
        let mut mixed_body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 3)],
            else_body: vec![assign_real(LocalId(1), 5.0)],
        }];
        let unknown_function = bool_condition_function(LirType::Unknown);
        let mut unknown_body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 3)],
            else_body: vec![assign_int(LocalId(1), 5)],
        }];

        assert!(!squash_branches_in_body(&mut real_body, &local_types(&real_function)));
        assert!(!squash_branches_in_body(&mut mixed_body, &local_types(&int_function)));
        assert!(!squash_branches_in_body(&mut unknown_body, &local_types(&unknown_function)));
    }

    #[test]
    fn branch_squash_replaces_flipped_bool_branch_with_not_condition() {
        let function = bool_condition_function(LirType::Bool);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), false)],
            else_body: vec![assign_bool(LocalId(1), true)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Unary { op: UnaryOp::Not, arg: Box::new(Expr::Local(LocalId(0))) },
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_unknown_direct_int_branch_with_cast_condition() {
        let function = bool_condition_function(LirType::Unknown);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 1)],
            else_body: vec![assign_int(LocalId(1), 0)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Unary {
                    op: UnaryOp::Cast(LirType::Int),
                    arg: Box::new(Expr::Local(LocalId(0))),
                },
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_unknown_flipped_int_branch_with_cast_not_condition() {
        let function = bool_condition_function(LirType::Unknown);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 0)],
            else_body: vec![assign_int(LocalId(1), 1)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Unary {
                    op: UnaryOp::Cast(LirType::Int),
                    arg: Box::new(Expr::Unary {
                        op: UnaryOp::Not,
                        arg: Box::new(Expr::Local(LocalId(0))),
                    }),
                },
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_unknown_direct_bool_branch_with_condition() {
        let function = bool_condition_function(LirType::Unknown);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), true)],
            else_body: vec![assign_bool(LocalId(1), false)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Local(LocalId(0)),
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_unknown_flipped_bool_branch_with_not_condition() {
        let function = bool_condition_function(LirType::Unknown);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), false)],
            else_body: vec![assign_bool(LocalId(1), true)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Unary { op: UnaryOp::Not, arg: Box::new(Expr::Local(LocalId(0))) },
            })]
        );
    }

    #[test]
    fn branch_squash_rejects_known_incompatible_destination_type() {
        let real_function = bool_condition_function(LirType::Real);
        let mut real_body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 1)],
            else_body: vec![assign_int(LocalId(1), 0)],
        }];
        let str_function = bool_condition_function(LirType::Str);
        let mut str_body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), true)],
            else_body: vec![assign_bool(LocalId(1), false)],
        }];

        assert!(!squash_branches_in_body(&mut real_body, &local_types(&real_function)));
        assert!(!squash_branches_in_body(&mut str_body, &local_types(&str_function)));
    }

    #[test]
    fn branch_squash_rejects_side_effecting_condition_and_different_targets() {
        let function = bool_condition_function(LirType::Int);
        let mut side_effecting = vec![StructuredStmt::If {
            cond: Expr::Call { target: "probe".to_owned(), args: Vec::new() },
            then_body: vec![assign_int(LocalId(1), 1)],
            else_body: vec![assign_int(LocalId(1), 0)],
        }];
        let mut different_targets = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 1)],
            else_body: vec![assign_int(LocalId(2), 0)],
        }];

        assert!(!squash_branches_in_body(&mut side_effecting, &local_types(&function)));
        assert!(!squash_branches_in_body(&mut different_targets, &local_types(&function)));
    }

    #[test]
    fn optional_live_ins_track_values_derived_from_undefined_locals() {
        let target = Label(3);
        let mut optional = HashSet::new();
        let mut defined = HashSet::new();
        let helper_params = HashMap::from([(target, vec![LocalId(1)])]);
        let body = vec![
            StructuredStmt::Stmt(Stmt::Assign { dst: LocalId(1), value: Expr::Local(LocalId(0)) }),
            StructuredStmt::CallHelper(target),
        ];

        collect_optional_helper_live_ins(&body, &mut defined, &helper_params, &mut optional);

        assert_eq!(optional, HashSet::from([(target, LocalId(1))]));
    }

    #[test]
    fn copy_alias_propagation_rewrites_straight_line_and_leaves_dce_removal() {
        let mut body = vec![
            StructuredStmt::Stmt(Stmt::Assign { dst: LocalId(1), value: Expr::Local(LocalId(0)) }),
            StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(Expr::Local(LocalId(1))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(1))),
                },
            }),
            StructuredStmt::Return(vec![ReturnValue::Named {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(2)),
            }]),
        ];

        assert!(propagate_copy_aliases_in_body(&mut body, &mut HashMap::new()));
        assert_eq!(
            body[1],
            StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(1))),
                },
            })
        );

        let mut live = LiveSet::new(3);
        assert!(prune_dead_assignments_in_body(&mut body, &mut live, &HashMap::new()));
        assert_eq!(
            body,
            vec![
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(2),
                    value: Expr::Binary {
                        op: BinaryOp::Add,
                        lhs: Box::new(Expr::Local(LocalId(0))),
                        rhs: Box::new(Expr::Const(ConstValue::Int(1))),
                    },
                }),
                StructuredStmt::Return(vec![ReturnValue::Named {
                    key: "x".to_owned(),
                    value: Expr::Local(LocalId(2)),
                }]),
            ]
        );
    }

    #[test]
    fn copy_alias_propagation_follows_transitive_aliases() {
        let mut body = vec![
            StructuredStmt::Stmt(Stmt::Assign { dst: LocalId(1), value: Expr::Local(LocalId(0)) }),
            StructuredStmt::Stmt(Stmt::Assign { dst: LocalId(2), value: Expr::Local(LocalId(1)) }),
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(2)),
            }),
        ];

        assert!(propagate_copy_aliases_in_body(&mut body, &mut HashMap::new()));
        assert_eq!(
            body[2],
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(0)),
            })
        );
    }

    #[test]
    fn copy_alias_propagation_keeps_branch_aliases_that_agree() {
        let mut body = vec![
            StructuredStmt::If {
                cond: Expr::Local(LocalId(3)),
                then_body: vec![StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Local(LocalId(0)),
                })],
                else_body: vec![StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Local(LocalId(0)),
                })],
            },
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(1)),
            }),
        ];

        assert!(propagate_copy_aliases_in_body(&mut body, &mut HashMap::new()));
        assert_eq!(
            body[1],
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(0)),
            })
        );
    }

    #[test]
    fn copy_alias_propagation_drops_branch_aliases_that_disagree() {
        let original_capture = StructuredStmt::Stmt(Stmt::Capture {
            key: "x".to_owned(),
            value: Expr::Local(LocalId(1)),
        });
        let mut body = vec![
            StructuredStmt::If {
                cond: Expr::Local(LocalId(3)),
                then_body: vec![StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Local(LocalId(0)),
                })],
                else_body: vec![StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Local(LocalId(2)),
                })],
            },
            original_capture.clone(),
        ];

        assert!(!propagate_copy_aliases_in_body(&mut body, &mut HashMap::new()));
        assert_eq!(body[1], original_capture);
    }

    #[test]
    fn copy_alias_propagation_kills_aliases_when_target_is_redefined() {
        let mut body = vec![
            StructuredStmt::Stmt(Stmt::Assign { dst: LocalId(1), value: Expr::Local(LocalId(0)) }),
            assign_int(LocalId(0), 3),
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(1)),
            }),
        ];

        assert!(!propagate_copy_aliases_in_body(&mut body, &mut HashMap::new()));
        assert_eq!(
            body[2],
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(1)),
            })
        );
    }

    #[test]
    fn copy_alias_propagation_rewrites_capture_operands() {
        let mut body = vec![
            StructuredStmt::Stmt(Stmt::Assign { dst: LocalId(1), value: Expr::Local(LocalId(0)) }),
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Binary {
                    op: BinaryOp::Mul,
                    lhs: Box::new(Expr::Local(LocalId(1))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(2))),
                },
            }),
        ];

        assert!(propagate_copy_aliases_in_body(&mut body, &mut HashMap::new()));
        assert_eq!(
            body[1],
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Binary {
                    op: BinaryOp::Mul,
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(2))),
                },
            })
        );
    }

    fn assign_int(dst: LocalId, value: i32) -> StructuredStmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value: Expr::Const(ConstValue::Int(value)) })
    }

    fn assign_bool(dst: LocalId, value: bool) -> StructuredStmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value: Expr::Const(ConstValue::Bool(value)) })
    }

    fn assign_real(dst: LocalId, value: f64) -> StructuredStmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value: Expr::Const(ConstValue::Real(value)) })
    }

    fn int_branch_arithmetic_expr(else_int: i32, delta: i32) -> Expr {
        Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::Const(ConstValue::Int(else_int))),
            rhs: Box::new(Expr::Binary {
                op: BinaryOp::Mul,
                lhs: Box::new(Expr::Unary {
                    op: UnaryOp::Cast(LirType::Int),
                    arg: Box::new(Expr::Local(LocalId(0))),
                }),
                rhs: Box::new(Expr::Const(ConstValue::Int(delta))),
            }),
        }
    }

    fn bool_condition_function(value_ty: LirType) -> Function {
        Function {
            name: "test".to_owned(),
            params: vec![LocalId(0)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "cond".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(1), name_hint: "x".to_owned(), ty: value_ty },
                Local { id: LocalId(2), name_hint: "y".to_owned(), ty: value_ty },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
            output_types: HashMap::new(),
        }
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
            output_types: HashMap::new(),
        }
    }
}
