use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::lir::{Expr, Function, Label, LocalId, ReturnValue, Stmt, Terminator};
use crate::lir_backward;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StructuredFunction {
    pub helpers: Vec<StructuredHelper>,
    pub body: Vec<StructuredStmt>,
    pub facts: lir_backward::BackwardFacts,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StructuredHelper {
    pub label: Label,
    pub params: Vec<LocalId>,
    pub body: Vec<StructuredStmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StructuredStmt {
    Stmt(Stmt),
    If { cond: Expr, then_body: Vec<StructuredStmt>, else_body: Vec<StructuredStmt> },
    CallHelper { label: Label, arg_hints: Vec<LocalId> },
    Return(Vec<ReturnValue>),
    Raise(&'static str),
}

impl StructuredStmt {
    pub(crate) fn call_helper(label: Label) -> Self {
        Self::CallHelper { label, arg_hints: Vec::new() }
    }
}

pub(crate) fn structure(function: &Function) -> Result<StructuredFunction> {
    let timing = std::env::var_os("MIR_LIFT_TIMING").is_some();
    let start = Instant::now();
    let plan = StructurePlan::new(function);
    if timing {
        eprintln!(
            "mir-lift timing: {} structure-plan {:?} helpers={}",
            function.name,
            start.elapsed(),
            plan.helpers.len()
        );
    }
    let builder = StructureBuilder { function, plan };

    let mut helpers = builder.plan.helpers.iter().copied().collect::<Vec<_>>();
    helpers.sort();

    let start = Instant::now();
    let helpers = helpers
        .into_iter()
        .map(|label| {
            let mut active = HashSet::new();
            active.insert(label);
            Ok(StructuredHelper {
                label,
                params: Vec::new(),
                body: builder.block_body(label, &mut active)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if timing {
        eprintln!("mir-lift timing: {} structure-helpers {:?}", function.name, start.elapsed());
    }

    let start = Instant::now();
    let body = if builder.plan.is_helper(function.entry) {
        vec![StructuredStmt::call_helper(function.entry)]
    } else {
        let mut active = HashSet::new();
        builder.block_body(function.entry, &mut active)?
    };
    if timing {
        eprintln!("mir-lift timing: {} structure-body {:?}", function.name, start.elapsed());
    }

    let mut structured = StructuredFunction { helpers, body, facts: Default::default() };
    let start = Instant::now();
    structured.facts = lir_backward::run_backward_passes(function, &mut structured);
    populate_helper_call_arg_hints(&mut structured);
    validate_helper_calls(&structured)?;
    if timing {
        eprintln!("mir-lift timing: {} structure-backward {:?}", function.name, start.elapsed());
    }
    Ok(structured)
}

fn validate_helper_calls(structured: &StructuredFunction) -> Result<()> {
    let defined = structured.helpers.iter().map(|helper| helper.label).collect::<HashSet<_>>();
    let mut referenced = HashSet::new();
    collect_helper_calls(&structured.body, &mut referenced);
    for helper in &structured.helpers {
        collect_helper_calls(&helper.body, &mut referenced);
    }

    if let Some(label) = referenced.into_iter().find(|label| !defined.contains(label)) {
        bail!("structured LIR references missing helper {label}");
    }

    Ok(())
}

fn collect_helper_calls(body: &[StructuredStmt], referenced: &mut HashSet<Label>) {
    for stmt in body {
        match stmt {
            StructuredStmt::CallHelper { label, .. } => {
                referenced.insert(*label);
            }
            StructuredStmt::If { then_body, else_body, .. } => {
                collect_helper_calls(then_body, referenced);
                collect_helper_calls(else_body, referenced);
            }
            StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => {}
        }
    }
}

fn populate_helper_call_arg_hints(structured: &mut StructuredFunction) {
    let helper_params = structured
        .helpers
        .iter()
        .map(|helper| (helper.label, helper.params.clone()))
        .collect::<HashMap<_, _>>();
    populate_helper_call_arg_hints_in_body(&mut structured.body, &helper_params);
    for helper in &mut structured.helpers {
        populate_helper_call_arg_hints_in_body(&mut helper.body, &helper_params);
    }
}

fn populate_helper_call_arg_hints_in_body(
    body: &mut [StructuredStmt],
    helper_params: &HashMap<Label, Vec<LocalId>>,
) {
    for stmt in body {
        match stmt {
            StructuredStmt::CallHelper { label, arg_hints } => {
                if let Some(params) = helper_params.get(label) {
                    if !lir_backward::helper_arg_hints_are_valid_read_set(arg_hints, params) {
                        *arg_hints = params.clone();
                    }
                }
            }
            StructuredStmt::If { then_body, else_body, .. } => {
                populate_helper_call_arg_hints_in_body(then_body, helper_params);
                populate_helper_call_arg_hints_in_body(else_body, helper_params);
            }
            StructuredStmt::Stmt(_) | StructuredStmt::Return(_) | StructuredStmt::Raise(_) => {}
        }
    }
}

struct StructurePlan {
    helpers: HashSet<Label>,
}

impl StructurePlan {
    fn new(function: &Function) -> Self {
        let mut incoming = HashMap::<Label, usize>::new();
        *incoming.entry(function.entry).or_insert(0) += 1;
        for block in &function.blocks {
            for target in successors(&block.term) {
                *incoming.entry(target).or_insert(0) += 1;
            }
        }

        let mut helpers = incoming
            .into_iter()
            .filter_map(|(label, count)| (count > 1).then_some(label))
            .collect::<HashSet<_>>();
        mark_cycle_targets(function, &mut helpers);

        Self { helpers }
    }

    fn is_helper(&self, label: Label) -> bool {
        self.helpers.contains(&label)
    }
}

struct StructureBuilder<'a> {
    function: &'a Function,
    plan: StructurePlan,
}

impl StructureBuilder<'_> {
    fn block_body(&self, label: Label, active: &mut HashSet<Label>) -> Result<Vec<StructuredStmt>> {
        let block = self
            .function
            .block(label)
            .with_context(|| format!("LIR references missing block {label}"))?;
        let mut body = block.stmts.iter().cloned().map(StructuredStmt::Stmt).collect::<Vec<_>>();
        self.append_terminator(&mut body, &block.term, active)?;
        Ok(body)
    }

    fn append_terminator(
        &self,
        body: &mut Vec<StructuredStmt>,
        term: &Terminator,
        active: &mut HashSet<Label>,
    ) -> Result<()> {
        match term {
            Terminator::Goto(label) => body.extend(self.transfer_body(*label, active)?),
            Terminator::Branch { cond, then_label, else_label } => {
                let mut then_active = active.clone();
                let mut else_active = active.clone();
                body.push(StructuredStmt::If {
                    cond: cond.clone(),
                    then_body: self.transfer_body(*then_label, &mut then_active)?,
                    else_body: self.transfer_body(*else_label, &mut else_active)?,
                });
            }
            Terminator::Return(values) => body.push(StructuredStmt::Return(values.clone())),
            Terminator::Unreachable => body.push(StructuredStmt::Raise("unreachable LIR block")),
        }
        Ok(())
    }

    fn transfer_body(
        &self,
        label: Label,
        active: &mut HashSet<Label>,
    ) -> Result<Vec<StructuredStmt>> {
        if self.plan.is_helper(label) {
            return Ok(vec![StructuredStmt::call_helper(label)]);
        }

        if active.contains(&label) {
            return Ok(vec![StructuredStmt::Raise("cyclic inline LIR block")]);
        }

        active.insert(label);
        let body = self.block_body(label, active);
        active.remove(&label);
        body
    }
}

fn successors(term: &Terminator) -> Vec<Label> {
    match term {
        Terminator::Goto(label) => vec![*label],
        Terminator::Branch { then_label, else_label, .. } => vec![*then_label, *else_label],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}

fn mark_cycle_targets(function: &Function, helpers: &mut HashSet<Label>) {
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    mark_cycle_targets_from(function, function.entry, helpers, &mut visiting, &mut visited);
}

fn mark_cycle_targets_from(
    function: &Function,
    label: Label,
    helpers: &mut HashSet<Label>,
    visiting: &mut HashSet<Label>,
    visited: &mut HashSet<Label>,
) {
    if visited.contains(&label) {
        return;
    }
    if !visiting.insert(label) {
        helpers.insert(label);
        return;
    }

    let Some(block) = function.block(label) else {
        visiting.remove(&label);
        visited.insert(label);
        return;
    };

    for target in successors(&block.term) {
        if visiting.contains(&target) {
            helpers.insert(target);
        } else {
            mark_cycle_targets_from(function, target, helpers, visiting, visited);
        }
    }

    visiting.remove(&label);
    visited.insert(label);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn populate_helper_call_arg_hints_preserves_strict_subsets() {
        let target = Label(9);
        let mut structured = StructuredFunction {
            body: vec![
                StructuredStmt::CallHelper { label: target, arg_hints: vec![LocalId(1)] },
                StructuredStmt::CallHelper {
                    label: target,
                    arg_hints: vec![LocalId(3), LocalId(1)],
                },
            ],
            helpers: vec![StructuredHelper {
                label: target,
                params: vec![LocalId(1), LocalId(3)],
                body: Vec::new(),
            }],
            facts: Default::default(),
        };

        populate_helper_call_arg_hints(&mut structured);

        assert_eq!(
            structured.body,
            vec![
                StructuredStmt::CallHelper { label: target, arg_hints: vec![LocalId(1)] },
                StructuredStmt::CallHelper {
                    label: target,
                    arg_hints: vec![LocalId(1), LocalId(3)],
                },
            ]
        );
    }
}
