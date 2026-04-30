use std::collections::HashMap;

use mir::{Block, Inst, InstructionData, Opcode, Value};

use crate::FunctionUnit;

#[derive(Default)]
pub(crate) struct ForwardFacts {
    pub aliases: ValueAliases,
}

#[derive(Default)]
pub(crate) struct ValueAliases {
    pub copies: HashMap<Value, Value>,
    pub exprs: HashMap<Value, AliasExpr>,
}

#[derive(Clone, Copy)]
pub(crate) enum AliasExpr {
    Unary { opcode: Opcode, arg: Value },
    Binary { opcode: Opcode, args: [Value; 2] },
}

pub(crate) fn run_forward_passes(unit: &FunctionUnit<'_>) -> ForwardFacts {
    let mut cx = ForwardPassCx { unit, facts: ForwardFacts::default() };
    run_forward_pass(&mut cx, &mut CopyAndInlineValues);
    cx.canonicalize_copy_aliases();
    cx.facts
}

pub(crate) trait ForwardMirPass {
    fn visit_phi_result(&mut self, _cx: &mut ForwardPassCx<'_>, _block: Block, _result: Value) {}
    fn visit_inst(
        &mut self,
        _cx: &mut ForwardPassCx<'_>,
        _block: Block,
        _inst: Inst,
        _data: &InstructionData,
        _results: &[Value],
    ) {
    }
    fn finish(&mut self, _cx: &mut ForwardPassCx<'_>) {}
}

pub(crate) struct ForwardPassCx<'a> {
    unit: &'a FunctionUnit<'a>,
    facts: ForwardFacts,
}

impl ForwardPassCx<'_> {
    pub(crate) fn unit(&self) -> &FunctionUnit<'_> {
        self.unit
    }

    pub(crate) fn facts(&self) -> &ForwardFacts {
        &self.facts
    }

    pub(crate) fn facts_mut(&mut self) -> &mut ForwardFacts {
        &mut self.facts
    }

    pub(crate) fn use_count_in_unit(&self, value: Value) -> usize {
        self.unit
            .source
            .dfg
            .values
            .uses(value)
            .filter(|use_| {
                let (parent, _) = self.unit.source.dfg.values.use_to_operand(*use_);
                self.unit
                    .source
                    .layout
                    .inst_block(parent)
                    .is_some_and(|block| self.unit.contains_block(block))
            })
            .count()
    }

    pub(crate) fn single_user_inst_in_unit(&self, value: Value) -> Option<Inst> {
        let uses = self
            .unit
            .source
            .dfg
            .values
            .uses(value)
            .filter_map(|use_| {
                let (parent, _) = self.unit.source.dfg.values.use_to_operand(use_);
                let parent_block = self.unit.source.layout.inst_block(parent)?;
                if self.unit.contains_block(parent_block) {
                    Some(parent)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let [inst] = uses.as_slice() else {
            return None;
        };
        Some(*inst)
    }

    fn canonicalize_copy_aliases(&mut self) {
        let keys = self.facts.aliases.copies.keys().copied().collect::<Vec<_>>();
        for key in keys {
            self.facts.aliases.copies.insert(key, self.canonical_copy_target(key));
        }
    }

    fn canonical_copy_target(&self, mut value: Value) -> Value {
        let mut seen = Vec::new();
        while let Some(&target) = self.facts.aliases.copies.get(&value) {
            if seen.contains(&value) || target == value {
                break;
            }
            seen.push(value);
            value = target;
        }
        value
    }
}

fn run_forward_pass(cx: &mut ForwardPassCx<'_>, pass: &mut dyn ForwardMirPass) {
    for &block in &cx.unit.blocks {
        for &inst in cx.unit.insts(block) {
            let data = &cx.unit.source.dfg.insts[inst];
            if let InstructionData::PhiNode(_) = data {
                for &result in cx.unit.source.dfg.inst_results(inst) {
                    pass.visit_phi_result(cx, block, result);
                }
                continue;
            }
            pass.visit_inst(cx, block, inst, data, cx.unit.source.dfg.inst_results(inst));
        }
    }
    pass.finish(cx);
}

struct CopyAndInlineValues;

impl ForwardMirPass for CopyAndInlineValues {
    fn visit_phi_result(&mut self, cx: &mut ForwardPassCx<'_>, _block: Block, result: Value) {
        let Some(use_inst) = cx.single_user_inst_in_unit(result) else {
            return;
        };
        if direct_copy_source(&cx.unit.source.dfg.insts[use_inst]) != Some(result) {
            return;
        }
        let [copy_result] = cx.unit.source.dfg.inst_results(use_inst) else {
            return;
        };
        cx.facts_mut().aliases.copies.insert(result, *copy_result);
    }

    fn visit_inst(
        &mut self,
        cx: &mut ForwardPassCx<'_>,
        _block: Block,
        _inst: Inst,
        data: &InstructionData,
        results: &[Value],
    ) {
        let [result] = results else {
            return;
        };
        if cx.facts().aliases.copies.contains_key(result) {
            return;
        }
        if let Some(source) = direct_copy_source(data) {
            if !cx.facts().aliases.copies.contains_key(&source) {
                cx.facts_mut().aliases.copies.insert(*result, source);
            }
            return;
        }
        if cx.use_count_in_unit(*result) != 1 {
            return;
        }
        match data {
            InstructionData::Unary { opcode, arg } if is_inline_unary_opcode(*opcode) => {
                cx.facts_mut()
                    .aliases
                    .exprs
                    .insert(*result, AliasExpr::Unary { opcode: *opcode, arg: *arg });
            }
            InstructionData::Binary { opcode, args } if is_inline_binary_opcode(*opcode) => {
                cx.facts_mut()
                    .aliases
                    .exprs
                    .insert(*result, AliasExpr::Binary { opcode: *opcode, args: *args });
            }
            _ => {}
        }
    }
}

pub(crate) fn direct_copy_source(data: &InstructionData) -> Option<Value> {
    match data {
        InstructionData::Unary { opcode: Opcode::OptBarrier, arg } => Some(*arg),
        _ => None,
    }
}

fn is_inline_unary_opcode(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Inot
            | Opcode::Bnot
            | Opcode::Fneg
            | Opcode::Ineg
            | Opcode::FIcast
            | Opcode::IFcast
            | Opcode::BIcast
            | Opcode::IBcast
            | Opcode::FBcast
            | Opcode::BFcast
            | Opcode::Sqrt
            | Opcode::Exp
            | Opcode::Ln
            | Opcode::Log
            | Opcode::Clog2
            | Opcode::Floor
            | Opcode::Ceil
            | Opcode::Sin
            | Opcode::Cos
            | Opcode::Tan
            | Opcode::Asin
            | Opcode::Acos
            | Opcode::Atan
            | Opcode::Sinh
            | Opcode::Cosh
            | Opcode::Tanh
            | Opcode::Asinh
            | Opcode::Acosh
            | Opcode::Atanh
    )
}

fn is_inline_binary_opcode(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Iadd
            | Opcode::Fadd
            | Opcode::Isub
            | Opcode::Fsub
            | Opcode::Imul
            | Opcode::Fmul
            | Opcode::Idiv
            | Opcode::Fdiv
            | Opcode::Irem
            | Opcode::Frem
            | Opcode::Ishl
            | Opcode::Ishr
            | Opcode::Ixor
            | Opcode::Iand
            | Opcode::Ior
            | Opcode::Ilt
            | Opcode::Flt
            | Opcode::Igt
            | Opcode::Fgt
            | Opcode::Ige
            | Opcode::Fge
            | Opcode::Ile
            | Opcode::Fle
            | Opcode::Ieq
            | Opcode::Feq
            | Opcode::Seq
            | Opcode::Beq
            | Opcode::Ine
            | Opcode::Fne
            | Opcode::Sne
            | Opcode::Bne
            | Opcode::Hypot
            | Opcode::Atan2
            | Opcode::Pow
    )
}
