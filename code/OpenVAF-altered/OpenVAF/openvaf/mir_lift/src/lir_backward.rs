use std::collections::{HashMap, HashSet};

use crate::lir::{
    BinaryOp, CallEffect, ConstValue, Expr, Function, Label, LirType, LocalId, ReturnValue, Stmt,
    UnaryOp,
};
use crate::lir_structure::{StructuredFunction, StructuredStmt};

const DEFAULT_CLEANUP_ROUNDS: usize = 12;
const DEFAULT_LATE_CLEANUP_ROUNDS: usize = 3;
const DEFAULT_POST_DCE_CLEANUP_ROUNDS: usize = 0;
const SMALL_HELPER_STMT_LIMIT: usize = 12;
const COST_INLINE_HELPER_CALL_LIMIT: usize = 3;
const COST_INLINE_MIN_SAVINGS: usize = 32;
const PUSH_UP_HELPER_CALL_LIMIT: usize = 4;
const PREDICATE_INLINE_EXPR_NODE_LIMIT: usize = 12;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackwardFacts {
    pub entry_live_ins: Vec<LocalId>,
    pub helper_live_ins: HashMap<Label, Vec<LocalId>>,
    pub optional_helper_live_ins: HashSet<(Label, LocalId)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BackwardRoundCounts {
    cleanup: usize,
    late_cleanup: usize,
    post_dce_cleanup: usize,
}

impl BackwardRoundCounts {
    fn from_env() -> Self {
        Self {
            cleanup: env_round_count("MIR_LIFT_LIR_CLEANUP_ROUNDS", DEFAULT_CLEANUP_ROUNDS),
            late_cleanup: env_round_count_with_fallback(
                "MIR_LIFT_LIR_LATE_CLEANUP_ROUNDS",
                "MIR_LIFT_LIR_FINAL_DCE_ROUNDS",
                DEFAULT_LATE_CLEANUP_ROUNDS,
            ),
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

fn env_round_count_with_fallback(name: &str, fallback_name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .or_else(|| std::env::var(fallback_name).ok())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
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
    BackwardPassKind::PredicateInlining,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::HelperComputationPushUp,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CopyAliasPropagation,
    BackwardPassKind::UnaryTempInlining,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::PredicateInlining,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::CopyAliasPropagation,
    BackwardPassKind::UnaryTempInlining,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::PredicateInlining,
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
const LATE_CLEANUP_PASSES: &[BackwardPassKind] = &[
    BackwardPassKind::DropNonSemanticEffects,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchMerge,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::AggressiveScalarSelectRecovery,
    BackwardPassKind::HelperSignaturePruning,
    BackwardPassKind::CopyAliasPropagation,
    BackwardPassKind::UnaryTempInlining,
    BackwardPassKind::DeadAssignments,
    BackwardPassKind::BranchInvariantHoist,
    BackwardPassKind::BranchMerge,
    BackwardPassKind::BranchSquash,
    BackwardPassKind::AggressiveScalarSelectRecovery,
    BackwardPassKind::CopyAliasPropagation,
    BackwardPassKind::UnaryTempInlining,
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
    let late_cleanup_pipeline =
        BackwardPipeline { name: "lir-backward-late-cleanup", passes: LATE_CLEANUP_PASSES };
    let rounds = BackwardRoundCounts::from_env();

    for _ in 0..rounds.cleanup {
        if !cleanup_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts }) {
            break;
        }
    }

    structural_pipeline.run(&mut BackwardPassCx { function, structured, facts: &mut facts });

    for _ in 0..rounds.late_cleanup {
        if !late_cleanup_pipeline.run(&mut BackwardPassCx {
            function,
            structured,
            facts: &mut facts,
        }) {
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
    BranchMerge,
    PredicateInlining,
    BranchSquash,
    AggressiveScalarSelectRecovery,
    HelperComputationPushUp,
    HelperLiveIns,
    OptionalHelperLiveIns,
    CopyAliasPropagation,
    UnaryTempInlining,
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
            Self::BranchMerge => "branch-merge",
            Self::PredicateInlining => "predicate-inlining",
            Self::BranchSquash => "branch-squash",
            Self::AggressiveScalarSelectRecovery => "aggressive-scalar-select-recovery",
            Self::HelperComputationPushUp => "helper-computation-push-up",
            Self::HelperLiveIns => "helper-live-ins",
            Self::OptionalHelperLiveIns => "optional-helper-live-ins",
            Self::CopyAliasPropagation => "copy-alias-propagation",
            Self::UnaryTempInlining => "unary-temp-inlining",
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
            Self::BranchMerge => run_backward_pass::<BranchMerge>(cx),
            Self::PredicateInlining => run_backward_pass::<PredicateInlining>(cx),
            Self::BranchSquash => run_backward_pass::<BranchSquash>(cx),
            Self::AggressiveScalarSelectRecovery => {
                run_backward_pass::<AggressiveScalarSelectRecovery>(cx)
            }
            Self::HelperComputationPushUp => run_backward_pass::<HelperComputationPushUp>(cx),
            Self::HelperLiveIns => run_backward_pass::<HelperLiveIns>(cx),
            Self::OptionalHelperLiveIns => run_backward_pass::<OptionalHelperLiveIns>(cx),
            Self::CopyAliasPropagation => run_backward_pass::<CopyAliasPropagation>(cx),
            Self::UnaryTempInlining => run_backward_pass::<UnaryTempInlining>(cx),
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
        Expr::Abs { arg } => 5 + expr_text_cost(function, arg),
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
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
struct BranchMerge;

impl BackwardLirPass for BranchMerge {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let local_types = local_types(cx.function);
        let mut changed =
            merge_matching_condition_branches_in_body(&mut cx.structured.body, &local_types);
        for helper in &mut cx.structured.helpers {
            changed |= merge_matching_condition_branches_in_body(&mut helper.body, &local_types);
        }
        changed
    }
}

fn merge_matching_condition_branches_in_body(
    body: &mut Vec<StructuredStmt>,
    local_types: &[LirType],
) -> bool {
    let mut changed = false;
    for stmt in body.iter_mut() {
        changed |= merge_matching_condition_branches_in_stmt(stmt, local_types);
    }

    let mut stmt_summaries = body.iter().map(summarize_structured_stmt).collect::<Vec<_>>();
    let mut index = 0;
    while index + 1 < body.len() {
        if let Some((second_index, merged, middle)) =
            merged_branch_pair(body, &stmt_summaries, index, local_types)
        {
            body[index] = merged;
            body.splice(index + 1..=second_index, middle);
            stmt_summaries = body.iter().map(summarize_structured_stmt).collect::<Vec<_>>();
            changed = true;
            continue;
        }
        index += 1;
    }

    changed
}

fn merge_matching_condition_branches_in_stmt(
    stmt: &mut StructuredStmt,
    local_types: &[LirType],
) -> bool {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            merge_matching_condition_branches_in_body(then_body, local_types)
                | merge_matching_condition_branches_in_body(else_body, local_types)
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    }
}

fn merged_branch_pair(
    body: &[StructuredStmt],
    stmt_summaries: &[RegionSummary],
    index: usize,
    local_types: &[LirType],
) -> Option<(usize, StructuredStmt, Vec<StructuredStmt>)> {
    let mut middle_summary = RegionSummary::default();
    for second_index in index + 1..body.len() {
        if !middle_summary.is_allowed_intervening_region() {
            return None;
        }
        if let Some((merged, middle)) =
            merged_branch_pair_at(body, index, second_index, local_types, &middle_summary)
        {
            return Some((second_index, merged, middle));
        }
        middle_summary.absorb(stmt_summaries[second_index].clone());
    }
    None
}

fn merged_branch_pair_at(
    body: &[StructuredStmt],
    index: usize,
    second_index: usize,
    local_types: &[LirType],
    middle_summary: &RegionSummary,
) -> Option<(StructuredStmt, Vec<StructuredStmt>)> {
    let StructuredStmt::If { cond: first_cond, then_body: first_then, else_body: first_else } =
        body.get(index)?
    else {
        return None;
    };
    let StructuredStmt::If { cond: second_cond, then_body: second_then, else_body: second_else } =
        body.get(second_index)?
    else {
        return None;
    };
    let middle = &body[index + 1..second_index];

    if first_cond != second_cond || !is_stable_branch_merge_condition(first_cond, local_types) {
        return None;
    }
    let first_then_summary = summarize_region(first_then);
    let first_else_summary = summarize_region(first_else);
    let second_then_summary = summarize_region(second_then);
    let second_else_summary = summarize_region(second_else);
    if !first_then_summary.is_allowed_branch_region()
        || !first_else_summary.is_allowed_branch_region()
        || !second_then_summary.is_allowed_branch_region()
        || !second_else_summary.is_allowed_branch_region()
    {
        return None;
    }
    let mut cond_locals = Vec::new();
    collect_expr_local_ids(first_cond, &mut cond_locals);
    if region_writes_any(&first_then_summary, &cond_locals)
        || region_writes_any(&first_else_summary, &cond_locals)
        || region_writes_any(middle_summary, &cond_locals)
    {
        return None;
    }
    if !separated_branch_merge_dependencies_are_safe(
        middle_summary,
        &second_then_summary,
        &second_else_summary,
    ) {
        return None;
    }

    let mut then_body = Vec::with_capacity(first_then.len() + second_then.len());
    then_body.extend(first_then.iter().cloned());
    then_body.extend(second_then.iter().cloned());

    let mut else_body = Vec::with_capacity(first_else.len() + second_else.len());
    else_body.extend(first_else.iter().cloned());
    else_body.extend(second_else.iter().cloned());

    Some((StructuredStmt::If { cond: first_cond.clone(), then_body, else_body }, middle.to_vec()))
}

fn separated_branch_merge_dependencies_are_safe(
    middle: &RegionSummary,
    second_then: &RegionSummary,
    second_else: &RegionSummary,
) -> bool {
    if middle.is_empty() {
        return true;
    }

    let mut branch_reads = second_then.reads.clone();
    branch_reads.extend(second_else.reads.iter().copied());

    let mut second_writes = second_then.writes.clone();
    second_writes.extend(second_else.writes.iter().copied());

    let mut second_capture_keys = second_then.capture_keys.clone();
    second_capture_keys.extend(second_else.capture_keys.iter().cloned());
    branch_reads.is_disjoint(&middle.writes)
        && second_writes.is_disjoint(&middle.reads)
        && second_writes.is_disjoint(&middle.writes)
        && second_capture_keys.is_disjoint(&middle.capture_keys)
}

fn is_stable_branch_merge_condition(expr: &Expr, local_types: &[LirType]) -> bool {
    if is_total_predicate_expr(expr, local_types) {
        return true;
    }

    matches!(expr, Expr::Local(local) if local_types.get(local.0).is_some())
}

#[derive(Clone, Debug, Default)]
struct RegionSummary {
    reads: HashSet<LocalId>,
    writes: HashSet<LocalId>,
    has_control_exit: bool,
    has_helper_call: bool,
    has_unsupported: bool,
    has_call_effect: bool,
    has_capture: bool,
    has_expr_side_effect: bool,
    capture_keys: HashSet<String>,
    stmt_count: usize,
}

impl RegionSummary {
    fn is_empty(&self) -> bool {
        self.stmt_count == 0
    }

    fn has_blocking_effect(&self) -> bool {
        self.has_control_exit
            || self.has_helper_call
            || self.has_unsupported
            || self.has_call_effect
            || self.has_expr_side_effect
    }

    fn is_allowed_branch_region(&self) -> bool {
        !self.has_blocking_effect()
    }

    fn is_allowed_intervening_region(&self) -> bool {
        !self.has_blocking_effect()
    }

    fn absorb(&mut self, other: RegionSummary) {
        self.reads.extend(other.reads);
        self.writes.extend(other.writes);
        self.has_control_exit |= other.has_control_exit;
        self.has_helper_call |= other.has_helper_call;
        self.has_unsupported |= other.has_unsupported;
        self.has_call_effect |= other.has_call_effect;
        self.has_capture |= other.has_capture;
        self.has_expr_side_effect |= other.has_expr_side_effect;
        self.capture_keys.extend(other.capture_keys);
        self.stmt_count += other.stmt_count;
    }
}

fn summarize_region(body: &[StructuredStmt]) -> RegionSummary {
    let mut summary = RegionSummary::default();
    for stmt in body {
        summary.absorb(summarize_structured_stmt(stmt));
    }
    summary
}

fn summarize_structured_stmt(stmt: &StructuredStmt) -> RegionSummary {
    let mut summary = RegionSummary { stmt_count: 1, ..RegionSummary::default() };
    match stmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value }) => {
            summary.writes.insert(*dst);
            collect_expr_locals_into_set(value, &mut summary.reads);
            summary.has_expr_side_effect |= expr_has_side_effects(value);
        }
        StructuredStmt::Stmt(Stmt::Capture { key, value }) => {
            collect_expr_locals_into_set(value, &mut summary.reads);
            summary.has_capture = true;
            summary.capture_keys.insert(key.clone());
            summary.has_expr_side_effect |= expr_has_side_effects(value);
        }
        StructuredStmt::Stmt(Stmt::CallEffect(effect)) => {
            collect_call_effect_locals_into_set(effect, &mut summary.reads);
            summary.has_call_effect = true;
        }
        StructuredStmt::Stmt(Stmt::Expr(value)) => {
            collect_expr_locals_into_set(value, &mut summary.reads);
            summary.has_expr_side_effect |= expr_has_side_effects(value);
        }
        StructuredStmt::Stmt(Stmt::Unsupported { dsts, .. }) => {
            summary.writes.extend(dsts.iter().copied());
            summary.has_unsupported = true;
        }
        StructuredStmt::If { cond, then_body, else_body } => {
            collect_expr_locals_into_set(cond, &mut summary.reads);
            summary.has_expr_side_effect |= expr_has_side_effects(cond);
            summary.absorb(summarize_region(then_body));
            summary.absorb(summarize_region(else_body));
        }
        StructuredStmt::CallHelper(_) => {
            summary.has_helper_call = true;
        }
        StructuredStmt::Return(values) => {
            for value in values {
                collect_return_value_locals_into_set(value, &mut summary.reads);
            }
            summary.has_control_exit = true;
        }
        StructuredStmt::Raise(_) => {
            summary.has_control_exit = true;
        }
    }
    summary
}

fn region_writes_any(summary: &RegionSummary, locals: &[LocalId]) -> bool {
    locals.iter().any(|local| summary.writes.contains(local))
}

fn collect_expr_locals_into_set(expr: &Expr, locals: &mut HashSet<LocalId>) {
    let mut ids = Vec::new();
    collect_expr_local_ids(expr, &mut ids);
    locals.extend(ids);
}

fn collect_return_value_locals_into_set(value: &ReturnValue, locals: &mut HashSet<LocalId>) {
    match value {
        ReturnValue::Named { value, .. } => collect_expr_locals_into_set(value, locals),
    }
}

fn collect_call_effect_locals_into_set(effect: &CallEffect, locals: &mut HashSet<LocalId>) {
    let mut ids = Vec::new();
    collect_call_effect_local_ids(effect, &mut ids);
    locals.extend(ids);
}

#[derive(Default)]
struct PredicateInlining;

impl BackwardLirPass for PredicateInlining {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let local_types = local_types(cx.function);
        let blocked_after = HashSet::new();
        let mut changed =
            inline_predicates_in_body(&mut cx.structured.body, &local_types, &blocked_after);
        for helper in &mut cx.structured.helpers {
            changed |= inline_predicates_in_body(&mut helper.body, &local_types, &blocked_after);
        }
        changed
    }
}

fn inline_predicates_in_body(
    body: &mut Vec<StructuredStmt>,
    local_types: &[LirType],
    blocked_after_body: &HashSet<LocalId>,
) -> bool {
    let mut changed = false;
    for index in 0..body.len() {
        let mut blocked_after_stmt = blocked_after_body.clone();
        if suffix_may_observe_locals(body, index + 1, &mut blocked_after_stmt) {
            continue;
        }
        if let StructuredStmt::If { then_body, else_body, .. } = &mut body[index] {
            changed |= inline_predicates_in_body(then_body, local_types, &blocked_after_stmt);
            changed |= inline_predicates_in_body(else_body, local_types, &blocked_after_stmt);
        }
    }

    let mut index = 0;
    while index + 1 < body.len() {
        if let Some((dst, value)) = inlineable_predicate_assignment(body, index, local_types) {
            if !blocked_after_body.contains(&dst)
                && !local_observed_before_redefinition(body, index + 2, dst)
            {
                if let StructuredStmt::If { cond, .. } = &mut body[index + 1] {
                    *cond = value;
                }
                body.remove(index);
                changed = true;
                continue;
            }
        }
        index += 1;
    }

    changed
}

fn inlineable_predicate_assignment(
    body: &[StructuredStmt],
    index: usize,
    local_types: &[LirType],
) -> Option<(LocalId, Expr)> {
    let StructuredStmt::Stmt(Stmt::Assign { dst, value }) = body.get(index)? else {
        return None;
    };
    let StructuredStmt::If { cond, then_body, else_body } = body.get(index + 1)? else {
        return None;
    };

    let (cond_local, inline_value) = match cond {
        Expr::Local(cond) => (*cond, value.clone()),
        Expr::Unary { op: UnaryOp::Not, arg } => match arg.as_ref() {
            Expr::Local(cond) => {
                (*cond, Expr::Unary { op: UnaryOp::Not, arg: Box::new(value.clone()) })
            }
            _ => return None,
        },
        _ => return None,
    };

    if *dst != cond_local {
        return None;
    }
    if body_has_control_boundary(then_body) || body_has_control_boundary(else_body) {
        return None;
    }
    if body_uses_local(then_body, *dst) || body_uses_local(else_body, *dst) {
        return None;
    }
    if expr_node_count(value) > PREDICATE_INLINE_EXPR_NODE_LIMIT {
        return None;
    }
    is_total_predicate_expr(value, local_types).then_some((*dst, inline_value))
}

fn suffix_may_observe_locals(
    body: &[StructuredStmt],
    start: usize,
    observed: &mut HashSet<LocalId>,
) -> bool {
    for stmt in body.iter().skip(start) {
        if stmt_has_control_boundary(stmt) {
            return true;
        }
        collect_structured_stmt_uses_into_set(stmt, observed);
    }
    false
}

fn local_observed_before_redefinition(
    body: &[StructuredStmt],
    start: usize,
    needle: LocalId,
) -> bool {
    for stmt in body.iter().skip(start) {
        if stmt_has_control_boundary(stmt) || structured_stmt_uses_local(stmt, needle) {
            return true;
        }
        if structured_stmt_redefines_local(stmt, needle) {
            return false;
        }
    }
    false
}

fn expr_node_count(expr: &Expr) -> usize {
    match expr {
        Expr::Local(_) | Expr::Const(_) => 1,
        Expr::Unary { arg, .. } => 1 + expr_node_count(arg),
        Expr::Binary { lhs, rhs, .. } => 1 + expr_node_count(lhs) + expr_node_count(rhs),
        Expr::Abs { arg } => 1 + expr_node_count(arg),
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
            1 + expr_node_count(lhs) + expr_node_count(rhs)
        }
        Expr::SimparamOpt { name, default } => 1 + expr_node_count(name) + expr_node_count(default),
        Expr::Call { args, .. } | Expr::Unsupported { args, .. } => {
            1 + args.iter().map(expr_node_count).sum::<usize>()
        }
    }
}

fn is_total_predicate_expr(expr: &Expr, local_types: &[LirType]) -> bool {
    if expr_has_side_effects(expr) || expr_type(expr, local_types) != Some(LirType::Bool) {
        return false;
    }

    match expr {
        Expr::Const(ConstValue::Bool(_)) => true,
        Expr::Local(local) => local_types.get(local.0) == Some(&LirType::Bool),
        Expr::Unary { op: UnaryOp::Not, arg } => is_total_predicate_expr(arg, local_types),
        Expr::Unary { op: UnaryOp::Cast(LirType::Bool), arg } => is_total_bool_cast_operand(arg),
        Expr::Binary { op: BinaryOp::BitAnd | BinaryOp::BitOr, lhs, rhs } => {
            is_total_predicate_expr(lhs, local_types) && is_total_predicate_expr(rhs, local_types)
        }
        Expr::Binary {
            op:
                BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge,
            lhs,
            rhs,
        } => {
            is_total_comparison_operand(lhs, local_types)
                && is_total_comparison_operand(rhs, local_types)
        }
        _ => false,
    }
}

fn is_total_bool_cast_operand(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Local(_)
            | Expr::Const(
                ConstValue::Bool(_)
                    | ConstValue::Int(_)
                    | ConstValue::Real(_)
                    | ConstValue::Str(_)
                    | ConstValue::None,
            )
    )
}

fn is_total_comparison_operand(expr: &Expr, local_types: &[LirType]) -> bool {
    match expr {
        Expr::Local(local) => matches!(
            local_types.get(local.0),
            Some(LirType::Bool | LirType::Int | LirType::Real | LirType::Str)
        ),
        Expr::Const(
            ConstValue::Bool(_) | ConstValue::Int(_) | ConstValue::Real(_) | ConstValue::Str(_),
        ) => true,
        _ => false,
    }
}

fn body_uses_local(body: &[StructuredStmt], needle: LocalId) -> bool {
    body.iter().any(|stmt| structured_stmt_uses_local(stmt, needle))
}

fn structured_stmt_uses_local(stmt: &StructuredStmt, needle: LocalId) -> bool {
    let mut uses = Vec::new();
    collect_structured_stmt_uses(stmt, &mut uses);
    uses.contains(&needle)
}

fn structured_stmt_redefines_local(stmt: &StructuredStmt, needle: LocalId) -> bool {
    matches!(stmt, StructuredStmt::Stmt(Stmt::Assign { dst, .. }) if *dst == needle)
}

fn collect_structured_stmt_uses_into_set(stmt: &StructuredStmt, uses: &mut HashSet<LocalId>) {
    let mut collected = Vec::new();
    collect_structured_stmt_uses(stmt, &mut collected);
    uses.extend(collected);
}

fn body_has_control_boundary(body: &[StructuredStmt]) -> bool {
    body.iter().any(stmt_has_control_boundary)
}

fn stmt_has_control_boundary(stmt: &StructuredStmt) -> bool {
    match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            body_has_control_boundary(then_body) || body_has_control_boundary(else_body)
        }
        StructuredStmt::CallHelper(_) => true,
        StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => false,
    }
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

fn squash_branches_in_body(body: &mut Vec<StructuredStmt>, local_types: &[LirType]) -> bool {
    let mut changed = false;
    for stmt in body.iter_mut() {
        changed |= squash_branches_in_stmt(stmt, local_types);
    }

    let mut index = 0;
    while index < body.len() {
        let Some(replacement) = squashed_branch_assignments(&body[index], local_types) else {
            index += 1;
            continue;
        };
        body.splice(index..=index, replacement);
        index += 1;
        changed = true;
    }
    changed
}

fn squash_branches_in_stmt(stmt: &mut StructuredStmt, local_types: &[LirType]) -> bool {
    let changed = match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            squash_branches_in_body(then_body, local_types)
                | squash_branches_in_body(else_body, local_types)
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    };

    changed
}

fn squashed_branch_assignments(
    stmt: &StructuredStmt,
    local_types: &[LirType],
) -> Option<Vec<StructuredStmt>> {
    let StructuredStmt::If { cond, then_body, else_body } = stmt else {
        return None;
    };
    if expr_has_side_effects(cond) || expr_type(cond, local_types) != Some(LirType::Bool) {
        return None;
    }
    if then_body.len() != else_body.len() || then_body.is_empty() {
        return None;
    }

    let is_multi_assignment = then_body.len() > 1;
    let mut replacements = Vec::with_capacity(then_body.len());
    for (then_stmt, else_stmt) in then_body.iter().zip(else_body) {
        let (dst, then_value) = assignment_stmt(then_stmt)?;
        let (else_dst, else_value) = assignment_stmt(else_stmt)?;
        if dst != else_dst {
            return None;
        }
        let dst_ty = *local_types.get(dst.0)?;
        let value =
            squashed_bool_expr_branch_expr(cond, then_value, else_value, dst_ty, local_types)
                .or_else(|| squashed_boolish_branch_expr(cond, then_value, else_value, dst_ty))
                .or_else(|| squashed_int_const_branch_expr(cond, then_value, else_value, dst_ty))?;
        if is_multi_assignment && expr_type(&value, local_types) != Some(LirType::Bool) {
            return None;
        }
        replacements.push(StructuredStmt::Stmt(Stmt::Assign { dst, value }));
    }

    Some(replacements)
}

fn squashed_bool_expr_branch_expr(
    cond: &Expr,
    then_value: &Expr,
    else_value: &Expr,
    dst_ty: LirType,
    _local_types: &[LirType],
) -> Option<Expr> {
    if !matches!(dst_ty, LirType::Bool | LirType::Unknown) {
        return None;
    }
    if !is_eager_safe_bool_expr(then_value) || !is_eager_safe_bool_expr(else_value) {
        return None;
    }

    let cond = bool_cast(cond.clone());
    match (bool_const_expr(then_value), bool_const_expr(else_value)) {
        (Some(true), None) => Some(bool_or(cond.clone(), else_value.clone())),
        (Some(false), None) => Some(bool_and(bool_not(cond.clone()), else_value.clone())),
        (None, Some(true)) => Some(bool_or(bool_not(cond.clone()), then_value.clone())),
        (None, Some(false)) => Some(bool_and(cond.clone(), then_value.clone())),
        _ => None,
    }
}

fn squashed_boolish_branch_expr(
    cond: &Expr,
    then_value: &Expr,
    else_value: &Expr,
    dst_ty: LirType,
) -> Option<Expr> {
    let (Expr::Const(then_value), Expr::Const(else_value)) = (then_value, else_value) else {
        return None;
    };
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
    then_value: &Expr,
    else_value: &Expr,
    dst_ty: LirType,
) -> Option<Expr> {
    if dst_ty != LirType::Int {
        return None;
    }
    let (Expr::Const(ConstValue::Int(then_int)), Expr::Const(ConstValue::Int(else_int))) =
        (then_value, else_value)
    else {
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

fn single_assignment(body: &[StructuredStmt]) -> Option<(LocalId, &Expr)> {
    let [StructuredStmt::Stmt(Stmt::Assign { dst, value })] = body else {
        return None;
    };
    Some((*dst, value))
}

fn assignment_stmt(stmt: &StructuredStmt) -> Option<(LocalId, &Expr)> {
    let StructuredStmt::Stmt(Stmt::Assign { dst, value }) = stmt else {
        return None;
    };
    Some((*dst, value))
}

fn bool_const_expr(expr: &Expr) -> Option<bool> {
    let Expr::Const(ConstValue::Bool(value)) = expr else {
        return None;
    };
    Some(*value)
}

fn is_eager_safe_bool_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Const(ConstValue::Bool(_)) => true,
        Expr::Unary { op: UnaryOp::Not, arg } => is_eager_safe_bool_expr(arg),
        Expr::Unary { op: UnaryOp::Cast(LirType::Bool), arg } => is_total_bool_cast_operand(arg),
        Expr::Binary { op: BinaryOp::BitAnd | BinaryOp::BitOr, lhs, rhs } => {
            is_eager_safe_bool_expr(lhs) && is_eager_safe_bool_expr(rhs)
        }
        _ => false,
    }
}

fn bool_not(arg: Expr) -> Expr {
    Expr::Unary { op: UnaryOp::Not, arg: Box::new(arg) }
}

fn bool_cast(arg: Expr) -> Expr {
    Expr::Unary { op: UnaryOp::Cast(LirType::Bool), arg: Box::new(arg) }
}

fn bool_and(lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary { op: BinaryOp::BitAnd, lhs: Box::new(lhs), rhs: Box::new(rhs) }
}

fn bool_or(lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary { op: BinaryOp::BitOr, lhs: Box::new(lhs), rhs: Box::new(rhs) }
}

#[derive(Default)]
struct AggressiveScalarSelectRecovery;

impl BackwardLirPass for AggressiveScalarSelectRecovery {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let local_types = local_types(cx.function);
        let mut changed =
            rewrite_aggressive_scalar_selects_in_body(&mut cx.structured.body, &local_types);
        for helper in &mut cx.structured.helpers {
            changed |= rewrite_aggressive_scalar_selects_in_body(&mut helper.body, &local_types);
        }
        changed
    }
}

fn rewrite_aggressive_scalar_selects_in_body(
    body: &mut [StructuredStmt],
    local_types: &[LirType],
) -> bool {
    let mut changed = false;
    for stmt in body {
        changed |= rewrite_aggressive_scalar_selects_in_stmt(stmt, local_types);
    }
    changed
}

fn rewrite_aggressive_scalar_selects_in_stmt(
    stmt: &mut StructuredStmt,
    local_types: &[LirType],
) -> bool {
    let mut changed = match stmt {
        StructuredStmt::If { then_body, else_body, .. } => {
            rewrite_aggressive_scalar_selects_in_body(then_body, local_types)
                | rewrite_aggressive_scalar_selects_in_body(else_body, local_types)
        }
        StructuredStmt::Stmt(_)
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Return(_)
        | StructuredStmt::Raise(_) => false,
    };

    if let Some(replacement) = aggressive_scalar_select_assignment(stmt, local_types) {
        *stmt = replacement;
        changed = true;
    }

    changed
}

fn aggressive_scalar_select_assignment(
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
    if dst != else_dst || local_types.get(dst.0).copied()? != LirType::Real {
        return None;
    }

    let value = max_min_branch_expr(cond, then_value, else_value, local_types)
        .or_else(|| abs_branch_expr(cond, then_value, else_value, local_types))?;
    Some(StructuredStmt::Stmt(Stmt::Assign { dst, value }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaxMinKind {
    Max,
    Min,
}

fn max_min_branch_expr(
    cond: &Expr,
    then_value: &Expr,
    else_value: &Expr,
    local_types: &[LirType],
) -> Option<Expr> {
    let Expr::Binary { op: op @ (BinaryOp::Gt | BinaryOp::Lt), lhs, rhs } = cond else {
        return None;
    };
    if !is_scalar_select_operand(lhs, local_types) || !is_scalar_select_operand(rhs, local_types) {
        return None;
    }
    if !is_same_expr(then_value, lhs) && !is_same_expr(then_value, rhs) {
        return None;
    }
    if !is_same_expr(else_value, lhs) && !is_same_expr(else_value, rhs) {
        return None;
    }

    let selects_lhs = is_same_expr(then_value, lhs) && is_same_expr(else_value, rhs);
    let selects_rhs = is_same_expr(then_value, rhs) && is_same_expr(else_value, lhs);
    let kind = match (op, selects_lhs, selects_rhs) {
        (BinaryOp::Gt, true, false) | (BinaryOp::Lt, false, true) => MaxMinKind::Max,
        (BinaryOp::Gt, false, true) | (BinaryOp::Lt, true, false) => MaxMinKind::Min,
        _ => return None,
    };

    let lhs = Box::new(else_value.clone());
    let rhs = Box::new(then_value.clone());
    match kind {
        MaxMinKind::Max => Some(Expr::Max { lhs, rhs }),
        MaxMinKind::Min => Some(Expr::Min { lhs, rhs }),
    }
}

fn is_scalar_select_operand(expr: &Expr, local_types: &[LirType]) -> bool {
    match expr {
        Expr::Local(local) => local_types.get(local.0).copied() == Some(LirType::Real),
        Expr::Const(ConstValue::Int(_) | ConstValue::Real(_)) => true,
        _ => false,
    }
}

fn is_same_expr(value: &Expr, expr: &Expr) -> bool {
    value == expr
}

fn abs_branch_expr(
    cond: &Expr,
    then_value: &Expr,
    else_value: &Expr,
    local_types: &[LirType],
) -> Option<Expr> {
    let (arg, op) = strict_zero_compare(cond, local_types)?;
    let then_polarity = abs_arm_polarity(then_value, arg)?;
    let else_polarity = abs_arm_polarity(else_value, arg)?;
    let is_math_abs_shape = matches!(
        (op, then_polarity, else_polarity),
        (BinaryOp::Lt, AbsArmPolarity::Neg, AbsArmPolarity::Pos)
            | (BinaryOp::Gt, AbsArmPolarity::Pos, AbsArmPolarity::Neg)
    );
    if !is_math_abs_shape {
        return None;
    }

    // Aggressive: strict-zero real diamonds are mathematically abs for ordinary nonzero numbers,
    // but their tie/NaN arms are observable in Python. Rewriting to abs(x) can change signed-zero
    // and NaN edge behavior; this pass accepts that tradeoff for path-count reduction.
    Some(Expr::Abs { arg: Box::new(Expr::Local(arg)) })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbsArmPolarity {
    Pos,
    Neg,
}

fn abs_arm_polarity(value: &Expr, arg: LocalId) -> Option<AbsArmPolarity> {
    match value {
        Expr::Local(local) if *local == arg => Some(AbsArmPolarity::Pos),
        Expr::Unary { op: UnaryOp::Neg, arg: neg_arg } => match neg_arg.as_ref() {
            Expr::Local(local) if *local == arg => Some(AbsArmPolarity::Neg),
            _ => None,
        },
        _ => None,
    }
}

fn strict_zero_compare(cond: &Expr, local_types: &[LirType]) -> Option<(LocalId, BinaryOp)> {
    let Expr::Binary { op: op @ (BinaryOp::Gt | BinaryOp::Lt), lhs, rhs } = cond else {
        return None;
    };

    match (lhs.as_ref(), rhs.as_ref()) {
        (Expr::Local(local), value) if is_zero_const(value) => {
            (local_types.get(local.0).copied()? == LirType::Real).then_some((*local, *op))
        }
        (value, Expr::Local(local)) if is_zero_const(value) => {
            if local_types.get(local.0).copied()? != LirType::Real {
                return None;
            }
            let flipped = match op {
                BinaryOp::Gt => BinaryOp::Lt,
                BinaryOp::Lt => BinaryOp::Gt,
                _ => return None,
            };
            Some((*local, flipped))
        }
        _ => None,
    }
}

fn is_zero_const(expr: &Expr) -> bool {
    match expr {
        Expr::Const(ConstValue::Int(0)) => true,
        Expr::Const(ConstValue::Real(value)) => *value == 0.0,
        _ => false,
    }
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
        Expr::Abs { arg } => expr_type(arg, local_types),
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
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
            Some(merge_expr_types(expr_type(lhs, local_types)?, expr_type(rhs, local_types)?))
        }
        Expr::SimparamOpt { default, .. } => expr_type(default, local_types),
        Expr::Call { .. } | Expr::Unsupported { .. } => None,
    }
}

fn merge_expr_types(lhs: LirType, rhs: LirType) -> LirType {
    match (lhs, rhs) {
        (same_lhs, same_rhs) if same_lhs == same_rhs => same_lhs,
        (LirType::Unknown, ty) | (ty, LirType::Unknown) => ty,
        (LirType::Real, LirType::Int | LirType::Bool)
        | (LirType::Int | LirType::Bool, LirType::Real) => LirType::Real,
        (LirType::Int, LirType::Bool) | (LirType::Bool, LirType::Int) => LirType::Int,
        _ => LirType::Unknown,
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
        Expr::Abs { arg } => rewrite_alias_expr(arg, aliases),
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_alias_expr(lhs, aliases) | rewrite_alias_expr(rhs, aliases)
        }
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
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
struct UnaryTempInlining;

impl BackwardLirPass for UnaryTempInlining {
    fn run(&mut self, cx: &mut BackwardPassCx<'_>) -> bool {
        let helper_params = cx.facts.helper_live_ins.clone();
        let entry_params = cx.function.params.iter().copied().collect();
        let mut changed =
            inline_unary_temps_in_body(&mut cx.structured.body, &helper_params, &entry_params);
        for helper in &mut cx.structured.helpers {
            let protected = helper.params.iter().copied().collect();
            changed |= inline_unary_temps_in_body(&mut helper.body, &helper_params, &protected);
        }
        changed
    }
}

fn inline_unary_temps_in_body(
    body: &mut Vec<StructuredStmt>,
    helper_params: &HashMap<Label, Vec<LocalId>>,
    protected_locals: &HashSet<LocalId>,
) -> bool {
    let mut changed = false;

    for stmt in body.iter_mut() {
        if let StructuredStmt::If { then_body, else_body, .. } = stmt {
            changed |= inline_unary_temps_in_body(then_body, helper_params, protected_locals);
            changed |= inline_unary_temps_in_body(else_body, helper_params, protected_locals);
        }
    }

    let mut index = 0;
    while index + 1 < body.len() {
        if let Some((dst, replacement)) = inlineable_unary_temp_assignment(body, index) {
            if !protected_locals.contains(&dst)
                && !unary_temp_observed_before_redefinition(body, index + 2, dst, helper_params)
                && replace_single_local_use_in_stmt(&mut body[index + 1], dst, &replacement)
            {
                body.remove(index);
                changed = true;
                continue;
            }
        }
        index += 1;
    }

    changed
}

fn inlineable_unary_temp_assignment(
    body: &[StructuredStmt],
    index: usize,
) -> Option<(LocalId, Expr)> {
    let StructuredStmt::Stmt(Stmt::Assign { dst, value }) = body.get(index)? else {
        return None;
    };
    let Expr::Unary { op, arg } = value else {
        return None;
    };
    match op {
        UnaryOp::Neg if matches!(arg.as_ref(), Expr::Local(_) | Expr::Const(_)) => {}
        _ => return None,
    }
    let next = body.get(index + 1)?;
    if single_rewrite_stmt_local_use_count(next, *dst) == 1 {
        Some((*dst, value.clone()))
    } else {
        None
    }
}

fn unary_temp_observed_before_redefinition(
    body: &[StructuredStmt],
    start: usize,
    needle: LocalId,
    _helper_params: &HashMap<Label, Vec<LocalId>>,
) -> bool {
    for stmt in body.iter().skip(start) {
        if structured_stmt_uses_local(stmt, needle) {
            return true;
        }
        if let StructuredStmt::CallHelper(_) = stmt {
            return true;
        }
        if structured_stmt_redefines_local(stmt, needle) {
            return false;
        }
    }
    false
}

fn single_rewrite_stmt_local_use_count(stmt: &StructuredStmt, needle: LocalId) -> usize {
    match stmt {
        StructuredStmt::Stmt(Stmt::Assign { value, .. })
        | StructuredStmt::Stmt(Stmt::Capture { value, .. }) => expr_local_use_count(value, needle),
        StructuredStmt::If { cond, then_body, else_body } => {
            if body_uses_local(then_body, needle) || body_uses_local(else_body, needle) {
                usize::MAX
            } else {
                expr_local_use_count(cond, needle)
            }
        }
        StructuredStmt::Return(values) => values
            .iter()
            .map(|value| match value {
                ReturnValue::Named { value, .. } => expr_local_use_count(value, needle),
            })
            .sum(),
        StructuredStmt::Stmt(Stmt::Expr(_))
        | StructuredStmt::Stmt(Stmt::CallEffect(_))
        | StructuredStmt::Stmt(Stmt::Unsupported { .. })
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Raise(_) => usize::MAX,
    }
}

fn replace_single_local_use_in_stmt(
    stmt: &mut StructuredStmt,
    needle: LocalId,
    replacement: &Expr,
) -> bool {
    match stmt {
        StructuredStmt::Stmt(Stmt::Assign { value, .. })
        | StructuredStmt::Stmt(Stmt::Capture { value, .. }) => {
            replace_single_local_use_in_expr(value, needle, replacement)
        }
        StructuredStmt::If { cond, .. } => {
            replace_single_local_use_in_expr(cond, needle, replacement)
        }
        StructuredStmt::Return(values) => {
            let mut changed = false;
            for value in values {
                match value {
                    ReturnValue::Named { value, .. } => {
                        changed |= replace_single_local_use_in_expr(value, needle, replacement);
                    }
                }
            }
            changed
        }
        StructuredStmt::Stmt(Stmt::Expr(_))
        | StructuredStmt::Stmt(Stmt::CallEffect(_))
        | StructuredStmt::Stmt(Stmt::Unsupported { .. })
        | StructuredStmt::CallHelper(_)
        | StructuredStmt::Raise(_) => false,
    }
}

fn replace_single_local_use_in_expr(expr: &mut Expr, needle: LocalId, replacement: &Expr) -> bool {
    if expr_local_use_count(expr, needle) != 1 {
        return false;
    }
    replace_local_use_in_expr(expr, needle, replacement)
}

fn replace_local_use_in_expr(expr: &mut Expr, needle: LocalId, replacement: &Expr) -> bool {
    match expr {
        Expr::Local(local) if *local == needle => {
            *expr = replacement.clone();
            true
        }
        Expr::Local(_) | Expr::Const(_) => false,
        Expr::Unary { arg, .. } => replace_local_use_in_expr(arg, needle, replacement),
        Expr::Abs { arg } => replace_local_use_in_expr(arg, needle, replacement),
        Expr::Binary { lhs, rhs, .. } => {
            replace_local_use_in_expr(lhs, needle, replacement)
                | replace_local_use_in_expr(rhs, needle, replacement)
        }
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
            replace_local_use_in_expr(lhs, needle, replacement)
                | replace_local_use_in_expr(rhs, needle, replacement)
        }
        Expr::SimparamOpt { name, default } => {
            replace_local_use_in_expr(name, needle, replacement)
                | replace_local_use_in_expr(default, needle, replacement)
        }
        Expr::Call { args, .. } | Expr::Unsupported { args, .. } => {
            let mut changed = false;
            for arg in args {
                changed |= replace_local_use_in_expr(arg, needle, replacement);
            }
            changed
        }
    }
}

fn expr_local_use_count(expr: &Expr, needle: LocalId) -> usize {
    match expr {
        Expr::Local(local) => usize::from(*local == needle),
        Expr::Const(_) => 0,
        Expr::Unary { arg, .. } => expr_local_use_count(arg, needle),
        Expr::Abs { arg } => expr_local_use_count(arg, needle),
        Expr::Binary { lhs, rhs, .. } => {
            expr_local_use_count(lhs, needle) + expr_local_use_count(rhs, needle)
        }
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
            expr_local_use_count(lhs, needle) + expr_local_use_count(rhs, needle)
        }
        Expr::SimparamOpt { name, default } => {
            expr_local_use_count(name, needle) + expr_local_use_count(default, needle)
        }
        Expr::Call { args, .. } | Expr::Unsupported { args, .. } => {
            args.iter().map(|arg| expr_local_use_count(arg, needle)).sum()
        }
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
        Expr::Abs { arg } => expr_has_side_effects(arg),
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
            expr_has_side_effects(lhs) || expr_has_side_effects(rhs)
        }
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
        Expr::Abs { arg } => collect_expr_locals(arg, locals),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_locals(lhs, locals);
            collect_expr_locals(rhs, locals);
        }
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
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
        Expr::Abs { arg } => collect_expr_local_ids(arg, locals),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_local_ids(lhs, locals);
            collect_expr_local_ids(rhs, locals);
        }
        Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
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
    fn branch_merge_combines_adjacent_equal_condition_ifs() {
        let function = branch_merge_function();
        let cond = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Local(LocalId(1))),
        };
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond,
                then_body: vec![assign_int(LocalId(2), 1), assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(2), 2), assign_int(LocalId(3), 4)],
            }]
        );
    }

    #[test]
    fn branch_merge_rejects_different_conditions() {
        let function = branch_merge_function();
        let original = vec![
            StructuredStmt::If {
                cond: Expr::Binary {
                    op: BinaryOp::Eq,
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(Expr::Local(LocalId(1))),
                },
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            StructuredStmt::If {
                cond: Expr::Binary {
                    op: BinaryOp::Ne,
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(Expr::Local(LocalId(1))),
                },
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_merge_preserves_assignment_and_capture_order() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![
                    assign_int(LocalId(2), 1),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "then_a".to_owned(),
                        value: Expr::Local(LocalId(2)),
                    }),
                ],
                else_body: vec![
                    assign_int(LocalId(2), 2),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "else_a".to_owned(),
                        value: Expr::Local(LocalId(2)),
                    }),
                ],
            },
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![
                    assign_int(LocalId(3), 3),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "then_b".to_owned(),
                        value: Expr::Local(LocalId(3)),
                    }),
                ],
                else_body: vec![
                    assign_int(LocalId(3), 4),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "else_b".to_owned(),
                        value: Expr::Local(LocalId(3)),
                    }),
                ],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond,
                then_body: vec![
                    assign_int(LocalId(2), 1),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "then_a".to_owned(),
                        value: Expr::Local(LocalId(2)),
                    }),
                    assign_int(LocalId(3), 3),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "then_b".to_owned(),
                        value: Expr::Local(LocalId(3)),
                    }),
                ],
                else_body: vec![
                    assign_int(LocalId(2), 2),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "else_a".to_owned(),
                        value: Expr::Local(LocalId(2)),
                    }),
                    assign_int(LocalId(3), 4),
                    StructuredStmt::Stmt(Stmt::Capture {
                        key: "else_b".to_owned(),
                        value: Expr::Local(LocalId(3)),
                    }),
                ],
            }]
        );
    }

    #[test]
    fn branch_merge_allows_nested_branch_arms_without_effects() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let nested = StructuredStmt::If {
            cond: Expr::Local(LocalId(6)),
            then_body: vec![assign_int(LocalId(2), 1)],
            else_body: vec![assign_int(LocalId(2), 2)],
        };
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![nested.clone()],
                else_body: vec![assign_int(LocalId(2), 3)],
            },
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(3), 4)],
                else_body: vec![assign_int(LocalId(3), 5)],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond,
                then_body: vec![nested, assign_int(LocalId(3), 4)],
                else_body: vec![assign_int(LocalId(2), 3), assign_int(LocalId(3), 5)],
            }]
        );
    }

    #[test]
    fn branch_merge_rejects_helper_and_return() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let invalid_arms = [
            vec![StructuredStmt::CallHelper(Label(9))],
            vec![StructuredStmt::Return(vec![ReturnValue::Named {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(2)),
            }])],
        ];

        for invalid_arm in invalid_arms {
            let original = vec![
                StructuredStmt::If {
                    cond: cond.clone(),
                    then_body: invalid_arm,
                    else_body: vec![assign_int(LocalId(2), 2)],
                },
                StructuredStmt::If {
                    cond: cond.clone(),
                    then_body: vec![assign_int(LocalId(3), 3)],
                    else_body: vec![assign_int(LocalId(3), 4)],
                },
            ];
            let mut body = original.clone();

            assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
            assert_eq!(body, original);
        }
    }

    #[test]
    fn branch_merge_handles_empty_branch_bodies() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let mut body = vec![
            StructuredStmt::If { cond: cond.clone(), then_body: Vec::new(), else_body: Vec::new() },
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond,
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            }]
        );
    }

    #[test]
    fn branch_merge_rejects_condition_redefinition_before_second_if() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let original = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_bool(LocalId(4), false)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            StructuredStmt::If {
                cond,
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_merge_combines_separated_equal_condition_ifs_when_middle_is_independent() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let middle = assign_int(LocalId(5), 9);
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            middle.clone(),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                StructuredStmt::If {
                    cond,
                    then_body: vec![assign_int(LocalId(2), 1), assign_int(LocalId(3), 3)],
                    else_body: vec![assign_int(LocalId(2), 2), assign_int(LocalId(3), 4)],
                },
                middle,
            ]
        );
    }

    #[test]
    fn branch_merge_combines_separated_equal_condition_ifs_across_safe_capture() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let middle = StructuredStmt::Stmt(Stmt::Capture {
            key: "mid".to_owned(),
            value: Expr::Local(LocalId(2)),
        });
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            middle.clone(),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![StructuredStmt::Stmt(Stmt::Capture {
                    key: "moved_then".to_owned(),
                    value: Expr::Local(LocalId(3)),
                })],
                else_body: vec![StructuredStmt::Stmt(Stmt::Capture {
                    key: "moved_else".to_owned(),
                    value: Expr::Local(LocalId(3)),
                })],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                StructuredStmt::If {
                    cond,
                    then_body: vec![
                        assign_int(LocalId(2), 1),
                        StructuredStmt::Stmt(Stmt::Capture {
                            key: "moved_then".to_owned(),
                            value: Expr::Local(LocalId(3)),
                        }),
                    ],
                    else_body: vec![
                        assign_int(LocalId(2), 2),
                        StructuredStmt::Stmt(Stmt::Capture {
                            key: "moved_else".to_owned(),
                            value: Expr::Local(LocalId(3)),
                        }),
                    ],
                },
                middle,
            ]
        );
    }

    #[test]
    fn branch_merge_rejects_separated_capture_same_key_reordering() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let middle = StructuredStmt::Stmt(Stmt::Capture {
            key: "same".to_owned(),
            value: Expr::Local(LocalId(2)),
        });
        let original = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            middle,
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![StructuredStmt::Stmt(Stmt::Capture {
                    key: "same".to_owned(),
                    value: Expr::Local(LocalId(3)),
                })],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_merge_combines_exact_same_non_bool_local_condition() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(0));
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond,
                then_body: vec![assign_int(LocalId(2), 1), assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(2), 2), assign_int(LocalId(3), 4)],
            }]
        );
    }

    #[test]
    fn branch_merge_combines_separated_with_nested_independent_middle() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let middle = StructuredStmt::If {
            cond: Expr::Local(LocalId(6)),
            then_body: vec![assign_int(LocalId(5), 9)],
            else_body: vec![assign_int(LocalId(5), 10)],
        };
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            middle.clone(),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                StructuredStmt::If {
                    cond,
                    then_body: vec![assign_int(LocalId(2), 1), assign_int(LocalId(3), 3)],
                    else_body: vec![assign_int(LocalId(2), 2), assign_int(LocalId(3), 4)],
                },
                middle,
            ]
        );
    }

    #[test]
    fn branch_merge_allows_separated_when_middle_only_writes_first_branch_read() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let mut body = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(5)))],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            assign_int(LocalId(5), 9),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];

        assert!(merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                StructuredStmt::If {
                    cond: cond.clone(),
                    then_body: vec![
                        assign_expr(LocalId(2), Expr::Local(LocalId(5))),
                        assign_int(LocalId(3), 3),
                    ],
                    else_body: vec![assign_int(LocalId(2), 2), assign_int(LocalId(3), 4)],
                },
                assign_int(LocalId(5), 9),
            ]
        );
    }

    #[test]
    fn branch_merge_rejects_separated_when_middle_writes_second_branch_read() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let original = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            assign_int(LocalId(5), 9),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_expr(LocalId(3), Expr::Local(LocalId(5)))],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_merge_rejects_separated_when_nested_middle_writes_second_branch_read() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let middle = StructuredStmt::If {
            cond: Expr::Local(LocalId(6)),
            then_body: vec![assign_int(LocalId(5), 9)],
            else_body: vec![assign_int(LocalId(5), 10)],
        };
        let original = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            middle,
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_expr(LocalId(3), Expr::Local(LocalId(5)))],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_merge_rejects_separated_when_middle_reads_second_branch_write() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let original = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            assign_expr(LocalId(5), Expr::Local(LocalId(3))),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(3), 3)],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_merge_rejects_separated_when_middle_and_second_branch_write_same_local() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let original = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            assign_int(LocalId(5), 9),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(5), 3)],
                else_body: vec![assign_int(LocalId(5), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_merge_rejects_separated_effectful_intervening_block() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let invalid_middle = [StructuredStmt::CallHelper(Label(9))];

        for middle in invalid_middle {
            let original = vec![
                StructuredStmt::If {
                    cond: cond.clone(),
                    then_body: vec![assign_int(LocalId(2), 1)],
                    else_body: vec![assign_int(LocalId(2), 2)],
                },
                middle,
                StructuredStmt::If {
                    cond: cond.clone(),
                    then_body: vec![assign_int(LocalId(3), 3)],
                    else_body: vec![assign_int(LocalId(3), 4)],
                },
            ];
            let mut body = original.clone();

            assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
            assert_eq!(body, original);
        }
    }

    #[test]
    fn branch_merge_rejects_separated_when_middle_writes_captured_value() {
        let function = branch_merge_function();
        let cond = Expr::Local(LocalId(4));
        let original = vec![
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 2)],
            },
            assign_int(LocalId(3), 9),
            StructuredStmt::If {
                cond: cond.clone(),
                then_body: vec![StructuredStmt::Stmt(Stmt::Capture {
                    key: "moved_then".to_owned(),
                    value: Expr::Local(LocalId(3)),
                })],
                else_body: vec![assign_int(LocalId(3), 4)],
            },
        ];
        let mut body = original.clone();

        assert!(!merge_matching_condition_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn predicate_inlining_replaces_single_use_total_bool_temp_before_if() {
        let function = predicate_function();
        let predicate = Expr::Binary {
            op: BinaryOp::Lt,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Local(LocalId(1))),
        };
        let mut body = vec![
            assign_expr(LocalId(2), predicate.clone()),
            StructuredStmt::If {
                cond: Expr::Local(LocalId(2)),
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
        ];

        assert!(inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond: predicate,
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            }]
        );
    }

    #[test]
    fn predicate_inlining_allows_repeated_local_ids_in_disjoint_regions() {
        let function = predicate_function();
        let first_predicate = Expr::Binary {
            op: BinaryOp::Lt,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Local(LocalId(1))),
        };
        let second_predicate = Expr::Binary {
            op: BinaryOp::Ne,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Const(ConstValue::Int(0))),
        };
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Binary {
                op: BinaryOp::Gt,
                lhs: Box::new(Expr::Local(LocalId(0))),
                rhs: Box::new(Expr::Const(ConstValue::Int(0))),
            },
            then_body: vec![
                assign_expr(LocalId(2), first_predicate.clone()),
                StructuredStmt::If {
                    cond: Expr::Local(LocalId(2)),
                    then_body: vec![assign_int(LocalId(3), 1)],
                    else_body: vec![assign_int(LocalId(3), 0)],
                },
            ],
            else_body: vec![
                assign_expr(LocalId(2), second_predicate.clone()),
                StructuredStmt::If {
                    cond: Expr::Local(LocalId(2)),
                    then_body: vec![assign_int(LocalId(3), 2)],
                    else_body: vec![assign_int(LocalId(3), 3)],
                },
            ],
        }];

        assert!(inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond: Expr::Binary {
                    op: BinaryOp::Gt,
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(0))),
                },
                then_body: vec![StructuredStmt::If {
                    cond: first_predicate,
                    then_body: vec![assign_int(LocalId(3), 1)],
                    else_body: vec![assign_int(LocalId(3), 0)],
                }],
                else_body: vec![StructuredStmt::If {
                    cond: second_predicate,
                    then_body: vec![assign_int(LocalId(3), 2)],
                    else_body: vec![assign_int(LocalId(3), 3)],
                }],
            }]
        );
    }

    #[test]
    fn predicate_inlining_replaces_negated_total_bool_temp_before_if() {
        let function = predicate_function();
        let predicate = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Local(LocalId(1))),
        };
        let mut body = vec![
            assign_expr(LocalId(2), predicate.clone()),
            StructuredStmt::If {
                cond: Expr::Unary { op: UnaryOp::Not, arg: Box::new(Expr::Local(LocalId(2))) },
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
        ];

        assert!(inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond: Expr::Unary { op: UnaryOp::Not, arg: Box::new(predicate) },
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            }]
        );
    }

    #[test]
    fn predicate_inlining_replaces_bool_cast_temp_before_if() {
        let function = predicate_function();
        let predicate = Expr::Unary {
            op: UnaryOp::Cast(LirType::Bool),
            arg: Box::new(Expr::Local(LocalId(0))),
        };
        let mut body = vec![
            assign_expr(LocalId(2), predicate.clone()),
            StructuredStmt::If {
                cond: Expr::Unary { op: UnaryOp::Not, arg: Box::new(Expr::Local(LocalId(2))) },
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
        ];

        assert!(inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(
            body,
            vec![StructuredStmt::If {
                cond: Expr::Unary { op: UnaryOp::Not, arg: Box::new(predicate) },
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            }]
        );
    }

    #[test]
    fn predicate_inlining_rejects_later_or_branch_uses() {
        let function = predicate_function();
        let predicate = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Local(LocalId(1))),
        };
        let original = vec![
            assign_expr(LocalId(2), predicate),
            StructuredStmt::If {
                cond: Expr::Local(LocalId(2)),
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
            StructuredStmt::Stmt(Stmt::Capture {
                key: "pred".to_owned(),
                value: Expr::Local(LocalId(2)),
            }),
        ];
        let mut body = original.clone();

        assert!(!inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(body, original);

        let original = vec![
            assign_expr(
                LocalId(2),
                Expr::Binary {
                    op: BinaryOp::Eq,
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(Expr::Local(LocalId(1))),
                },
            ),
            StructuredStmt::If {
                cond: Expr::Local(LocalId(2)),
                then_body: vec![StructuredStmt::Stmt(Stmt::Capture {
                    key: "pred".to_owned(),
                    value: Expr::Local(LocalId(2)),
                })],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
        ];
        let mut body = original.clone();

        assert!(!inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(body, original);
    }

    #[test]
    fn predicate_inlining_rejects_nested_temp_live_after_parent_if() {
        let function = predicate_function();
        let predicate = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Local(LocalId(1))),
        };
        let original = vec![
            StructuredStmt::If {
                cond: Expr::Binary {
                    op: BinaryOp::Gt,
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(0))),
                },
                then_body: vec![
                    assign_expr(LocalId(2), predicate),
                    StructuredStmt::If {
                        cond: Expr::Local(LocalId(2)),
                        then_body: vec![assign_int(LocalId(3), 1)],
                        else_body: vec![assign_int(LocalId(3), 0)],
                    },
                ],
                else_body: vec![assign_int(LocalId(3), 2)],
            },
            StructuredStmt::Stmt(Stmt::Capture {
                key: "pred".to_owned(),
                value: Expr::Local(LocalId(2)),
            }),
        ];
        let mut body = original.clone();

        assert!(!inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(body, original);
    }

    #[test]
    fn predicate_inlining_rejects_branch_control_boundaries() {
        let function = predicate_function();
        let predicate = Expr::Binary {
            op: BinaryOp::Eq,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Local(LocalId(1))),
        };
        let original = vec![
            assign_expr(LocalId(2), predicate),
            StructuredStmt::If {
                cond: Expr::Local(LocalId(2)),
                then_body: vec![StructuredStmt::CallHelper(Label(7))],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
        ];
        let mut body = original.clone();

        assert!(!inline_predicates_in_body(&mut body, &local_types(&function), &HashSet::new(),));
        assert_eq!(body, original);
    }

    #[test]
    fn predicate_inlining_rejects_calls_and_non_total_comparison_operands() {
        let function = predicate_function();
        let mut with_call = vec![
            assign_expr(LocalId(2), Expr::Call { target: "probe".to_owned(), args: Vec::new() }),
            StructuredStmt::If {
                cond: Expr::Local(LocalId(2)),
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
        ];
        let mut with_div_comparison = vec![
            assign_expr(
                LocalId(2),
                Expr::Binary {
                    op: BinaryOp::Lt,
                    lhs: Box::new(Expr::Binary {
                        op: BinaryOp::Div,
                        lhs: Box::new(Expr::Local(LocalId(0))),
                        rhs: Box::new(Expr::Local(LocalId(1))),
                    }),
                    rhs: Box::new(Expr::Const(ConstValue::Int(1))),
                },
            ),
            StructuredStmt::If {
                cond: Expr::Local(LocalId(2)),
                then_body: vec![assign_int(LocalId(3), 1)],
                else_body: vec![assign_int(LocalId(3), 0)],
            },
        ];

        for body in [&mut with_call, &mut with_div_comparison] {
            assert!(!inline_predicates_in_body(body, &local_types(&function), &HashSet::new(),));
        }
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
    fn branch_squash_rejects_true_then_bool_local_else_to_preserve_short_circuiting() {
        let function = bool_condition_function(LirType::Bool);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), true)],
            else_body: vec![assign_expr(LocalId(1), Expr::Local(LocalId(2)))],
        }];
        let original = body.clone();

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_squash_rejects_false_then_bool_local_else_to_preserve_short_circuiting() {
        let function = bool_condition_function(LirType::Bool);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), false)],
            else_body: vec![assign_expr(LocalId(1), Expr::Local(LocalId(2)))],
        }];
        let original = body.clone();

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_squash_rejects_bool_local_then_true_else_to_preserve_short_circuiting() {
        let function = bool_condition_function(LirType::Bool);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_expr(LocalId(1), Expr::Local(LocalId(2)))],
            else_body: vec![assign_bool(LocalId(1), true)],
        }];
        let original = body.clone();

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_squash_rejects_bool_local_then_false_else_to_preserve_short_circuiting() {
        let function = bool_condition_function(LirType::Bool);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_expr(LocalId(1), Expr::Local(LocalId(2)))],
            else_body: vec![assign_bool(LocalId(1), false)],
        }];
        let original = body.clone();

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_squash_rejects_comparison_bool_expr_arm_to_preserve_short_circuiting() {
        let function = Function {
            name: "test".to_owned(),
            params: vec![LocalId(0)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "value".to_owned(), ty: LirType::Int },
                Local { id: LocalId(1), name_hint: "dst".to_owned(), ty: LirType::Bool },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
            output_types: HashMap::new(),
        };
        let cond = Expr::Binary {
            op: BinaryOp::Ne,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Const(ConstValue::Int(0))),
        };
        let arm = Expr::Binary {
            op: BinaryOp::Ne,
            lhs: Box::new(Expr::Local(LocalId(0))),
            rhs: Box::new(Expr::Const(ConstValue::Int(1))),
        };
        let mut body = vec![StructuredStmt::If {
            cond: cond.clone(),
            then_body: vec![assign_expr(LocalId(1), arm.clone())],
            else_body: vec![assign_bool(LocalId(1), false)],
        }];
        let original = body.clone();

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn branch_squash_rejects_effectful_and_non_bool_expr_arms() {
        let bool_function = bool_condition_function(LirType::Bool);
        let mut effectful = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), true)],
            else_body: vec![assign_expr(
                LocalId(1),
                Expr::Call { target: "probe".to_owned(), args: Vec::new() },
            )],
        }];
        let int_function = bool_condition_function(LirType::Int);
        let mut non_bool = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), true)],
            else_body: vec![assign_expr(LocalId(1), Expr::Local(LocalId(2)))],
        }];

        assert!(!squash_branches_in_body(&mut effectful, &local_types(&bool_function)));
        assert!(!squash_branches_in_body(&mut non_bool, &local_types(&int_function)));
    }

    #[test]
    fn branch_squash_rejects_comparison_over_arithmetic_bool_expr_arm() {
        let function = Function {
            name: "test".to_owned(),
            params: vec![LocalId(0)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "cond".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(1), name_hint: "dst".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(2), name_hint: "value".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
            output_types: HashMap::new(),
        };
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), true)],
            else_body: vec![assign_expr(
                LocalId(1),
                Expr::Binary {
                    op: BinaryOp::Eq,
                    lhs: Box::new(Expr::Binary {
                        op: BinaryOp::Div,
                        lhs: Box::new(Expr::Local(LocalId(2))),
                        rhs: Box::new(Expr::Const(ConstValue::Int(0))),
                    }),
                    rhs: Box::new(Expr::Const(ConstValue::Int(1))),
                },
            )],
        }];
        let original = body.clone();

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
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
    fn branch_squash_replaces_bool_cast_then_false_else_with_condition_and_cast() {
        let function = bool_condition_function(LirType::Bool);
        let cast = Expr::Unary {
            op: UnaryOp::Cast(LirType::Bool),
            arg: Box::new(Expr::Local(LocalId(2))),
        };
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_expr(LocalId(1), cast.clone())],
            else_body: vec![assign_bool(LocalId(1), false)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Binary {
                    op: BinaryOp::BitAnd,
                    lhs: Box::new(Expr::Unary {
                        op: UnaryOp::Cast(LirType::Bool),
                        arg: Box::new(Expr::Local(LocalId(0))),
                    }),
                    rhs: Box::new(cast),
                },
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_false_then_bool_cast_else_with_not_condition_and_cast() {
        let function = bool_condition_function(LirType::Bool);
        let cast = Expr::Unary {
            op: UnaryOp::Cast(LirType::Bool),
            arg: Box::new(Expr::Local(LocalId(2))),
        };
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), false)],
            else_body: vec![assign_expr(LocalId(1), cast.clone())],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(1),
                value: Expr::Binary {
                    op: BinaryOp::BitAnd,
                    lhs: Box::new(Expr::Unary {
                        op: UnaryOp::Not,
                        arg: Box::new(Expr::Unary {
                            op: UnaryOp::Cast(LirType::Bool),
                            arg: Box::new(Expr::Local(LocalId(0))),
                        }),
                    }),
                    rhs: Box::new(cast),
                },
            })]
        );
    }

    #[test]
    fn branch_squash_replaces_multi_bool_expr_then_false_else_with_ands() {
        let function = branch_squash_bool_function();
        let cast_2 = bool_cast_local(LocalId(2));
        let cast_4 = bool_cast_local(LocalId(4));
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![
                assign_expr(LocalId(1), cast_2.clone()),
                assign_expr(LocalId(3), cast_4.clone()),
            ],
            else_body: vec![assign_bool(LocalId(1), false), assign_bool(LocalId(3), false)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Binary {
                        op: BinaryOp::BitAnd,
                        lhs: Box::new(Expr::Unary {
                            op: UnaryOp::Cast(LirType::Bool),
                            arg: Box::new(Expr::Local(LocalId(0))),
                        }),
                        rhs: Box::new(cast_2),
                    },
                }),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Binary {
                        op: BinaryOp::BitAnd,
                        lhs: Box::new(Expr::Unary {
                            op: UnaryOp::Cast(LirType::Bool),
                            arg: Box::new(Expr::Local(LocalId(0))),
                        }),
                        rhs: Box::new(cast_4),
                    },
                }),
            ]
        );
    }

    #[test]
    fn branch_squash_replaces_multi_bool_expr_false_then_else_with_negated_ands() {
        let function = branch_squash_bool_function();
        let cast_2 = bool_cast_local(LocalId(2));
        let cast_4 = bool_cast_local(LocalId(4));
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), false), assign_bool(LocalId(3), false)],
            else_body: vec![
                assign_expr(LocalId(1), cast_2.clone()),
                assign_expr(LocalId(3), cast_4.clone()),
            ],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Binary {
                        op: BinaryOp::BitAnd,
                        lhs: Box::new(Expr::Unary {
                            op: UnaryOp::Not,
                            arg: Box::new(Expr::Unary {
                                op: UnaryOp::Cast(LirType::Bool),
                                arg: Box::new(Expr::Local(LocalId(0))),
                            }),
                        }),
                        rhs: Box::new(cast_2),
                    },
                }),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Binary {
                        op: BinaryOp::BitAnd,
                        lhs: Box::new(Expr::Unary {
                            op: UnaryOp::Not,
                            arg: Box::new(Expr::Unary {
                                op: UnaryOp::Cast(LirType::Bool),
                                arg: Box::new(Expr::Local(LocalId(0))),
                            }),
                        }),
                        rhs: Box::new(cast_4),
                    },
                }),
            ]
        );
    }

    #[test]
    fn branch_squash_replaces_multi_bool_expr_with_or_forms() {
        let function = branch_squash_bool_function();
        let cast_2 = bool_cast_local(LocalId(2));
        let cast_4 = bool_cast_local(LocalId(4));
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_bool(LocalId(1), true), assign_expr(LocalId(3), cast_4.clone())],
            else_body: vec![assign_expr(LocalId(1), cast_2.clone()), assign_bool(LocalId(3), true)],
        }];

        assert!(squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(1),
                    value: Expr::Binary {
                        op: BinaryOp::BitOr,
                        lhs: Box::new(Expr::Unary {
                            op: UnaryOp::Cast(LirType::Bool),
                            arg: Box::new(Expr::Local(LocalId(0))),
                        }),
                        rhs: Box::new(cast_2),
                    },
                }),
                StructuredStmt::Stmt(Stmt::Assign {
                    dst: LocalId(3),
                    value: Expr::Binary {
                        op: BinaryOp::BitOr,
                        lhs: Box::new(Expr::Unary {
                            op: UnaryOp::Not,
                            arg: Box::new(Expr::Unary {
                                op: UnaryOp::Cast(LirType::Bool),
                                arg: Box::new(Expr::Local(LocalId(0))),
                            }),
                        }),
                        rhs: Box::new(cast_4),
                    },
                }),
            ]
        );
    }

    #[test]
    fn branch_squash_rejects_multi_bool_mismatched_destinations_and_effects() {
        let function = branch_squash_bool_function();
        let int_function = bool_condition_function(LirType::Int);
        let mut mismatched = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_expr(LocalId(1), bool_cast_local(LocalId(2)))],
            else_body: vec![assign_bool(LocalId(3), false)],
        }];
        let mut effectful = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![
                assign_expr(LocalId(1), bool_cast_local(LocalId(2))),
                StructuredStmt::Stmt(Stmt::Capture {
                    key: "x".to_owned(),
                    value: Expr::Local(LocalId(2)),
                }),
            ],
            else_body: vec![assign_bool(LocalId(1), false), assign_bool(LocalId(3), false)],
        }];
        let mut non_bool = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_int(LocalId(1), 1), assign_int(LocalId(2), 3)],
            else_body: vec![assign_int(LocalId(1), 0), assign_int(LocalId(2), 5)],
        }];
        let mismatched_original = mismatched.clone();
        let effectful_original = effectful.clone();
        let non_bool_original = non_bool.clone();

        assert!(!squash_branches_in_body(&mut mismatched, &local_types(&function)));
        assert_eq!(mismatched, mismatched_original);
        assert!(!squash_branches_in_body(&mut effectful, &local_types(&function)));
        assert_eq!(effectful, effectful_original);
        assert!(!squash_branches_in_body(&mut non_bool, &local_types(&int_function)));
        assert_eq!(non_bool, non_bool_original);
    }

    #[test]
    fn branch_squash_rejects_bool_cast_over_arithmetic_arm() {
        let function = bool_condition_function(LirType::Bool);
        let mut body = vec![StructuredStmt::If {
            cond: Expr::Local(LocalId(0)),
            then_body: vec![assign_expr(
                LocalId(1),
                Expr::Unary {
                    op: UnaryOp::Cast(LirType::Bool),
                    arg: Box::new(Expr::Binary {
                        op: BinaryOp::Div,
                        lhs: Box::new(Expr::Local(LocalId(2))),
                        rhs: Box::new(Expr::Const(ConstValue::Int(0))),
                    }),
                },
            )],
            else_body: vec![assign_bool(LocalId(1), false)],
        }];
        let original = body.clone();

        assert!(!squash_branches_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
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
    fn aggressive_scalar_select_rewrites_greater_select_to_max_with_false_arm_first() {
        let function = max_min_function();
        let mut body = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Gt, LocalId(0), LocalId(1)),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(1)))],
        }];

        assert!(rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: max_expr(LocalId(1), LocalId(0)),
            })]
        );
    }

    #[test]
    fn aggressive_scalar_select_rewrites_less_select_to_min_with_false_arm_first() {
        let function = max_min_function();
        let mut body = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Lt, LocalId(0), LocalId(1)),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(1)))],
        }];

        assert!(rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types(&function)));
        assert_eq!(
            body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: min_expr(LocalId(1), LocalId(0)),
            })]
        );
    }

    #[test]
    fn aggressive_scalar_select_handles_flipped_min_max_branches() {
        let function = max_min_function();
        let mut min_body = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Gt, LocalId(0), LocalId(1)),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(1)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
        }];
        let mut max_body = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Lt, LocalId(0), LocalId(1)),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(1)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
        }];

        assert!(rewrite_aggressive_scalar_selects_in_body(&mut min_body, &local_types(&function)));
        assert!(rewrite_aggressive_scalar_selects_in_body(&mut max_body, &local_types(&function)));
        assert_eq!(
            min_body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: min_expr(LocalId(0), LocalId(1)),
            })]
        );
        assert_eq!(
            max_body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: max_expr(LocalId(0), LocalId(1)),
            })]
        );
    }

    #[test]
    fn aggressive_scalar_select_rejects_extra_branch_statements() {
        let function = max_min_function();
        let original = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Gt, LocalId(0), LocalId(1)),
            then_body: vec![
                assign_expr(LocalId(2), Expr::Local(LocalId(0))),
                assign_real(LocalId(3), 0.0),
            ],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(1)))],
        }];
        let mut body = original.clone();

        assert!(!rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn aggressive_scalar_select_rejects_different_assignment_targets() {
        let function = max_min_function();
        let original = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Gt, LocalId(0), LocalId(1)),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(3), Expr::Local(LocalId(1)))],
        }];
        let mut body = original.clone();

        assert!(!rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn aggressive_scalar_select_rejects_non_strict_or_non_operand_selects() {
        let function = max_min_function();
        let mut non_strict = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Ge, LocalId(0), LocalId(1)),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(1)))],
        }];
        let original_non_strict = non_strict.clone();
        let mut non_operand = vec![StructuredStmt::If {
            cond: compare(BinaryOp::Gt, LocalId(0), LocalId(1)),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(3)))],
        }];
        let original_non_operand = non_operand.clone();

        assert!(!rewrite_aggressive_scalar_selects_in_body(
            &mut non_strict,
            &local_types(&function)
        ));
        assert_eq!(non_strict, original_non_strict);
        assert!(!rewrite_aggressive_scalar_selects_in_body(
            &mut non_operand,
            &local_types(&function)
        ));
        assert_eq!(non_operand, original_non_operand);
    }

    #[test]
    fn aggressive_scalar_select_rejects_side_effecting_condition() {
        let function = max_min_function();
        let original = vec![StructuredStmt::If {
            cond: Expr::Call { target: "probe".to_owned(), args: Vec::new() },
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(1)))],
        }];
        let mut body = original.clone();

        assert!(!rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn aggressive_scalar_select_recovers_local_constant_min_max() {
        let function = max_min_function();
        let c = Expr::Const(ConstValue::Real(1.5));
        let mut max_body = vec![StructuredStmt::If {
            cond: compare_expr(BinaryOp::Lt, Expr::Local(LocalId(0)), c.clone()),
            then_body: vec![assign_expr(LocalId(2), c.clone())],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
        }];
        let mut min_body = vec![StructuredStmt::If {
            cond: compare_expr(BinaryOp::Gt, Expr::Local(LocalId(0)), c.clone()),
            then_body: vec![assign_expr(LocalId(2), c.clone())],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
        }];

        assert!(rewrite_aggressive_scalar_selects_in_body(&mut max_body, &local_types(&function)));
        assert!(rewrite_aggressive_scalar_selects_in_body(&mut min_body, &local_types(&function)));
        assert_eq!(
            max_body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: Expr::Max {
                    lhs: Box::new(Expr::Local(LocalId(0))),
                    rhs: Box::new(c.clone()),
                },
            })]
        );
        assert_eq!(
            min_body,
            vec![StructuredStmt::Stmt(Stmt::Assign {
                dst: LocalId(2),
                value: Expr::Min { lhs: Box::new(Expr::Local(LocalId(0))), rhs: Box::new(c) },
            })]
        );
    }

    #[test]
    fn aggressive_scalar_select_recovers_abs_strict_zero_shapes() {
        let function = max_min_function();
        let local_types = local_types(&function);
        let cases = [
            (
                StructuredStmt::If {
                    cond: compare_expr(
                        BinaryOp::Lt,
                        Expr::Local(LocalId(0)),
                        Expr::Const(ConstValue::Real(0.0)),
                    ),
                    then_body: vec![assign_expr(LocalId(2), neg_local(LocalId(0)))],
                    else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
                },
                Expr::Abs { arg: Box::new(Expr::Local(LocalId(0))) },
            ),
            (
                StructuredStmt::If {
                    cond: compare_expr(
                        BinaryOp::Gt,
                        Expr::Local(LocalId(0)),
                        Expr::Const(ConstValue::Real(0.0)),
                    ),
                    then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
                    else_body: vec![assign_expr(LocalId(2), neg_local(LocalId(0)))],
                },
                Expr::Abs { arg: Box::new(Expr::Local(LocalId(0))) },
            ),
            (
                StructuredStmt::If {
                    cond: compare_expr(
                        BinaryOp::Gt,
                        Expr::Const(ConstValue::Int(0)),
                        Expr::Local(LocalId(0)),
                    ),
                    then_body: vec![assign_expr(LocalId(2), neg_local(LocalId(0)))],
                    else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
                },
                Expr::Abs { arg: Box::new(Expr::Local(LocalId(0))) },
            ),
        ];

        for (original, expected) in cases {
            let mut body = vec![original];
            assert!(rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types));
            assert_eq!(
                body,
                vec![StructuredStmt::Stmt(Stmt::Assign { dst: LocalId(2), value: expected })]
            );
        }
    }

    #[test]
    fn aggressive_scalar_select_rejects_abs_non_strict_and_non_exact_negation() {
        let function = max_min_function();
        let local_types = local_types(&function);
        let mut non_strict = vec![StructuredStmt::If {
            cond: compare_expr(
                BinaryOp::Le,
                Expr::Local(LocalId(0)),
                Expr::Const(ConstValue::Real(0.0)),
            ),
            then_body: vec![assign_expr(LocalId(2), neg_local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
        }];
        let original_non_strict = non_strict.clone();
        let mut non_exact_negation = vec![StructuredStmt::If {
            cond: compare_expr(
                BinaryOp::Lt,
                Expr::Local(LocalId(0)),
                Expr::Const(ConstValue::Real(0.0)),
            ),
            then_body: vec![assign_expr(LocalId(2), neg_local(LocalId(1)))],
            else_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
        }];
        let original_non_exact_negation = non_exact_negation.clone();

        assert!(!rewrite_aggressive_scalar_selects_in_body(&mut non_strict, &local_types));
        assert_eq!(non_strict, original_non_strict);
        assert!(!rewrite_aggressive_scalar_selects_in_body(&mut non_exact_negation, &local_types));
        assert_eq!(non_exact_negation, original_non_exact_negation);
    }

    #[test]
    fn aggressive_scalar_select_rejects_multi_statement_abs_derivative_bundle() {
        let function = max_min_function();
        let original = vec![StructuredStmt::If {
            cond: compare_expr(
                BinaryOp::Lt,
                Expr::Local(LocalId(0)),
                Expr::Const(ConstValue::Real(0.0)),
            ),
            then_body: vec![
                assign_expr(LocalId(2), neg_local(LocalId(0))),
                assign_real(LocalId(3), -1.0),
            ],
            else_body: vec![
                assign_expr(LocalId(2), Expr::Local(LocalId(0))),
                assign_real(LocalId(3), 1.0),
            ],
        }];
        let mut body = original.clone();

        assert!(!rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types(&function)));
        assert_eq!(body, original);
    }

    #[test]
    fn aggressive_scalar_select_rejects_non_abs_strict_zero_shapes() {
        let function = max_min_function();
        let local_types = local_types(&function);
        let cases = [StructuredStmt::If {
            cond: compare_expr(
                BinaryOp::Lt,
                Expr::Local(LocalId(0)),
                Expr::Const(ConstValue::Real(0.0)),
            ),
            then_body: vec![assign_expr(LocalId(2), Expr::Local(LocalId(0)))],
            else_body: vec![assign_expr(LocalId(2), neg_local(LocalId(0)))],
        }];

        for original in cases {
            let mut body = vec![original.clone()];
            assert!(!rewrite_aggressive_scalar_selects_in_body(&mut body, &local_types));
            assert_eq!(body, vec![original]);
        }
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
    fn unary_temp_inlining_rewrites_adjacent_assignment_use() {
        let neg = Expr::Unary { op: UnaryOp::Neg, arg: Box::new(Expr::Local(LocalId(0))) };
        let mut body = vec![
            assign_expr(LocalId(1), neg.clone()),
            assign_expr(
                LocalId(2),
                Expr::Binary {
                    op: BinaryOp::Mul,
                    lhs: Box::new(Expr::Local(LocalId(1))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(2))),
                },
            ),
        ];

        assert!(inline_unary_temps_in_body(&mut body, &HashMap::new(), &HashSet::new()));
        assert_eq!(
            body,
            vec![assign_expr(
                LocalId(2),
                Expr::Binary {
                    op: BinaryOp::Mul,
                    lhs: Box::new(neg),
                    rhs: Box::new(Expr::Const(ConstValue::Int(2))),
                },
            )]
        );
    }

    #[test]
    fn unary_temp_inlining_rewrites_capture_return_and_if_condition() {
        let neg = Expr::Unary { op: UnaryOp::Neg, arg: Box::new(Expr::Local(LocalId(0))) };

        let mut capture_body = vec![
            assign_expr(LocalId(1), neg.clone()),
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(1)),
            }),
        ];
        assert!(inline_unary_temps_in_body(&mut capture_body, &HashMap::new(), &HashSet::new()));
        assert_eq!(
            capture_body,
            vec![StructuredStmt::Stmt(Stmt::Capture { key: "x".to_owned(), value: neg.clone() })]
        );

        let mut return_body = vec![
            assign_expr(LocalId(1), neg.clone()),
            StructuredStmt::Return(vec![ReturnValue::Named {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(1)),
            }]),
        ];
        assert!(inline_unary_temps_in_body(&mut return_body, &HashMap::new(), &HashSet::new()));
        assert_eq!(
            return_body,
            vec![StructuredStmt::Return(vec![ReturnValue::Named {
                key: "x".to_owned(),
                value: neg.clone(),
            }])]
        );

        let mut if_body = vec![
            assign_expr(LocalId(1), neg.clone()),
            StructuredStmt::If {
                cond: Expr::Binary {
                    op: BinaryOp::Lt,
                    lhs: Box::new(Expr::Local(LocalId(1))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(0))),
                },
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 0)],
            },
        ];
        assert!(inline_unary_temps_in_body(&mut if_body, &HashMap::new(), &HashSet::new()));
        assert_eq!(
            if_body,
            vec![StructuredStmt::If {
                cond: Expr::Binary {
                    op: BinaryOp::Lt,
                    lhs: Box::new(neg),
                    rhs: Box::new(Expr::Const(ConstValue::Int(0))),
                },
                then_body: vec![assign_int(LocalId(2), 1)],
                else_body: vec![assign_int(LocalId(2), 0)],
            }]
        );
    }

    #[test]
    fn unary_temp_inlining_rejects_multiple_use_and_non_adjacent_cases() {
        let neg = Expr::Unary { op: UnaryOp::Neg, arg: Box::new(Expr::Local(LocalId(0))) };
        let original = vec![
            assign_expr(LocalId(1), neg.clone()),
            assign_expr(
                LocalId(2),
                Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(Expr::Local(LocalId(1))),
                    rhs: Box::new(Expr::Local(LocalId(1))),
                },
            ),
        ];
        let mut body = original.clone();
        assert!(!inline_unary_temps_in_body(&mut body, &HashMap::new(), &HashSet::new()));
        assert_eq!(body, original);

        let original = vec![
            assign_expr(LocalId(1), neg),
            assign_int(LocalId(3), 4),
            assign_expr(LocalId(2), Expr::Local(LocalId(1))),
        ];
        let mut body = original.clone();
        assert!(!inline_unary_temps_in_body(&mut body, &HashMap::new(), &HashSet::new()));
        assert_eq!(body, original);
    }

    #[test]
    fn unary_temp_inlining_rejects_protected_helper_or_entry_params() {
        let neg = Expr::Unary { op: UnaryOp::Neg, arg: Box::new(Expr::Local(LocalId(0))) };
        let original = vec![
            assign_expr(LocalId(1), neg),
            assign_expr(
                LocalId(2),
                Expr::Binary {
                    op: BinaryOp::Mul,
                    lhs: Box::new(Expr::Local(LocalId(1))),
                    rhs: Box::new(Expr::Const(ConstValue::Int(2))),
                },
            ),
        ];
        let mut body = original.clone();
        let protected = HashSet::from([LocalId(1)]);

        assert!(!inline_unary_temps_in_body(&mut body, &HashMap::new(), &protected));
        assert_eq!(body, original);
    }

    #[test]
    fn unary_temp_inlining_rejects_helper_boundary_and_later_use() {
        let neg = Expr::Unary { op: UnaryOp::Neg, arg: Box::new(Expr::Local(LocalId(0))) };
        let original = vec![
            assign_expr(LocalId(1), neg.clone()),
            StructuredStmt::CallHelper(Label(2)),
            assign_expr(LocalId(2), Expr::Local(LocalId(1))),
        ];
        let mut body = original.clone();
        let helper_params = HashMap::from([(Label(2), vec![LocalId(1)])]);
        assert!(!inline_unary_temps_in_body(&mut body, &helper_params, &HashSet::new()));
        assert_eq!(body, original);

        let original = vec![
            assign_expr(LocalId(1), neg),
            assign_expr(LocalId(2), Expr::Local(LocalId(1))),
            StructuredStmt::Stmt(Stmt::Capture {
                key: "x".to_owned(),
                value: Expr::Local(LocalId(1)),
            }),
        ];
        let mut body = original.clone();
        assert!(!inline_unary_temps_in_body(&mut body, &HashMap::new(), &HashSet::new()));
        assert_eq!(body, original);
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

    fn assign_expr(dst: LocalId, value: Expr) -> StructuredStmt {
        StructuredStmt::Stmt(Stmt::Assign { dst, value })
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

    fn compare(op: BinaryOp, lhs: LocalId, rhs: LocalId) -> Expr {
        Expr::Binary { op, lhs: Box::new(Expr::Local(lhs)), rhs: Box::new(Expr::Local(rhs)) }
    }

    fn compare_expr(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }
    }

    fn neg_local(local: LocalId) -> Expr {
        Expr::Unary { op: UnaryOp::Neg, arg: Box::new(Expr::Local(local)) }
    }

    fn max_expr(lhs: LocalId, rhs: LocalId) -> Expr {
        Expr::Max { lhs: Box::new(Expr::Local(lhs)), rhs: Box::new(Expr::Local(rhs)) }
    }

    fn min_expr(lhs: LocalId, rhs: LocalId) -> Expr {
        Expr::Min { lhs: Box::new(Expr::Local(lhs)), rhs: Box::new(Expr::Local(rhs)) }
    }

    fn bool_cast_local(local: LocalId) -> Expr {
        Expr::Unary { op: UnaryOp::Cast(LirType::Bool), arg: Box::new(Expr::Local(local)) }
    }

    fn max_min_function() -> Function {
        Function {
            name: "test".to_owned(),
            params: vec![LocalId(0), LocalId(1)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "x".to_owned(), ty: LirType::Real },
                Local { id: LocalId(1), name_hint: "y".to_owned(), ty: LirType::Real },
                Local { id: LocalId(2), name_hint: "res".to_owned(), ty: LirType::Real },
                Local { id: LocalId(3), name_hint: "other".to_owned(), ty: LirType::Real },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
            output_types: HashMap::new(),
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

    fn branch_squash_bool_function() -> Function {
        Function {
            name: "test".to_owned(),
            params: vec![LocalId(0)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "cond".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(1), name_hint: "x".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(2), name_hint: "raw_x".to_owned(), ty: LirType::Int },
                Local { id: LocalId(3), name_hint: "y".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(4), name_hint: "raw_y".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
            output_types: HashMap::new(),
        }
    }

    fn predicate_function() -> Function {
        Function {
            name: "test".to_owned(),
            params: vec![LocalId(0), LocalId(1)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "a".to_owned(), ty: LirType::Int },
                Local { id: LocalId(1), name_hint: "b".to_owned(), ty: LirType::Int },
                Local { id: LocalId(2), name_hint: "pred".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(3), name_hint: "out".to_owned(), ty: LirType::Int },
            ],
            entry: Label(0),
            blocks: Vec::new(),
            returns: Vec::new(),
            output_types: HashMap::new(),
        }
    }

    fn branch_merge_function() -> Function {
        Function {
            name: "test".to_owned(),
            params: vec![LocalId(0), LocalId(1), LocalId(4)],
            locals: vec![
                Local { id: LocalId(0), name_hint: "a".to_owned(), ty: LirType::Int },
                Local { id: LocalId(1), name_hint: "b".to_owned(), ty: LirType::Int },
                Local { id: LocalId(2), name_hint: "x".to_owned(), ty: LirType::Int },
                Local { id: LocalId(3), name_hint: "y".to_owned(), ty: LirType::Int },
                Local { id: LocalId(4), name_hint: "cond".to_owned(), ty: LirType::Bool },
                Local { id: LocalId(5), name_hint: "mid".to_owned(), ty: LirType::Int },
                Local { id: LocalId(6), name_hint: "other_cond".to_owned(), ty: LirType::Bool },
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
