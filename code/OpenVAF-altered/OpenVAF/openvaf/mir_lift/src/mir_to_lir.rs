use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use hir_lower::{CallBackKind, ParamInfoKind};
use lasso::Resolver;
use mir::{Block, Const, FuncRef, Function, Inst, InstructionData, Opcode, Value, ValueDef};

use crate::lir::{
    BinaryOp, CallEffect, ConstValue, Expr, Label, LirType, Local, LocalId, MathBinary, MathUnary,
    ReturnSlot, ReturnValue, Stmt, Terminator, UnaryOp,
};
use crate::mir_forward::{direct_copy_source, run_forward_passes, AliasExpr, ForwardFacts};
use crate::FunctionUnit;

pub(crate) fn lower_unit(
    unit: &FunctionUnit<'_>,
    resolver: &dyn Resolver,
) -> Result<crate::lir::Function> {
    let mut cx = LowerCx::new(unit, resolver);
    cx.lower()
}

struct LowerCx<'a, 'r> {
    unit: &'a FunctionUnit<'a>,
    resolver: &'r dyn Resolver,
    locals: Vec<Local>,
    locals_by_value: HashMap<Value, LocalId>,
    variable_touched_by_value: HashMap<Value, bool>,
    variable_touched_in_progress: HashSet<Value>,
    local_variable_touched: HashMap<LocalId, bool>,
    local_generic_temp_name: HashMap<LocalId, bool>,
    facts: ForwardFacts,
    labels_by_block: HashMap<Block, Label>,
    blocks: Vec<crate::lir::Block>,
}

impl<'a, 'r> LowerCx<'a, 'r> {
    fn new(unit: &'a FunctionUnit<'a>, resolver: &'r dyn Resolver) -> Self {
        let labels_by_block =
            unit.blocks.iter().enumerate().map(|(index, block)| (*block, Label(index))).collect();

        let facts = run_forward_passes(unit);

        Self {
            unit,
            resolver,
            locals: Vec::new(),
            locals_by_value: HashMap::new(),
            variable_touched_by_value: HashMap::new(),
            variable_touched_in_progress: HashSet::new(),
            local_variable_touched: HashMap::new(),
            local_generic_temp_name: HashMap::new(),
            facts,
            labels_by_block,
            blocks: Vec::new(),
        }
    }

    fn lower(&mut self) -> Result<crate::lir::Function> {
        for &param in &self.unit.params {
            self.local_for_value(param);
        }
        for &block in &self.unit.blocks {
            for &inst in self.unit.insts(block) {
                for &result in self.unit.source.dfg.inst_results(inst) {
                    if self.facts.aliases.exprs.contains_key(&result) {
                        continue;
                    }
                    self.local_for_result(result);
                }
            }
        }

        let params =
            self.unit.params.iter().map(|value| self.local_for_value(*value)).collect::<Vec<_>>();
        let returns = self
            .unit
            .return_values
            .iter()
            .filter_map(|value| {
                self.local_id_for_return(*value).map(|local| ReturnSlot {
                    key: self.output_key_for_value(*value),
                    value: local,
                })
            })
            .collect();
        let output_types = self.output_types();

        for &block in &self.unit.blocks {
            self.lower_block(block)?;
        }

        Ok(crate::lir::Function {
            name: self.unit.name.clone(),
            params,
            locals: self.locals.clone(),
            local_variable_touched: self.local_variable_touched.clone(),
            local_generic_temp_name: self.local_generic_temp_name.clone(),
            entry: self.labels_by_block[&self.unit.entry],
            blocks: self.blocks.clone(),
            returns,
            output_types,
        })
    }

    fn lower_block(&mut self, block: Block) -> Result<()> {
        let mut stmts = Vec::new();
        if block == self.unit.entry {
            self.capture_entry_values(&mut stmts);
        }
        for &inst in self.unit.insts(block) {
            let data = self.unit.source.dfg.insts[inst].clone();
            if data.is_phi() || data.is_terminator() {
                continue;
            }
            self.lower_inst(inst, data, &mut stmts)?;
        }

        let term = match self.real_block_terminator(block) {
            Some(InstructionData::Jump { destination })
                if self.unit.contains_block(destination) =>
            {
                Terminator::Goto(self.edge_label_or_block(block, destination))
            }
            Some(InstructionData::Branch { cond, then_dst, else_dst, .. })
                if self.unit.contains_block(then_dst) && self.unit.contains_block(else_dst) =>
            {
                Terminator::Branch {
                    cond: self.expr_for_value(cond),
                    then_label: self.edge_label_or_block(block, then_dst),
                    else_label: self.edge_label_or_block(block, else_dst),
                }
            }
            Some(InstructionData::Jump { .. } | InstructionData::Branch { .. }) => {
                Terminator::Return(self.return_values())
            }
            Some(InstructionData::Exit) | None => Terminator::Return(self.return_values()),
            Some(other) => {
                bail!("unsupported MIR terminator in {}: {other:?}", self.unit.name);
            }
        };

        self.blocks.push(crate::lir::Block { label: self.labels_by_block[&block], stmts, term });
        Ok(())
    }

    fn capture_entry_values(&mut self, stmts: &mut Vec<Stmt>) {
        for &value in &self.unit.capture_values {
            match self.unit.source.dfg.value_def(value) {
                ValueDef::Param(_) | ValueDef::Const(_) => {
                    let expr = self.expr_for_value(value);
                    self.capture_value_from_expr(value, expr, stmts);
                }
                ValueDef::Result(_, _) | ValueDef::Invalid => {}
            }
        }
    }

    fn real_block_terminator(&self, block: Block) -> Option<InstructionData> {
        self.unit
            .insts(block)
            .iter()
            .rev()
            .map(|inst| self.unit.source.dfg.insts[*inst].clone())
            .find(|data| data.is_terminator())
    }

    fn lower_inst(
        &mut self,
        inst: Inst,
        data: InstructionData,
        stmts: &mut Vec<Stmt>,
    ) -> Result<()> {
        let rendered = self.unit.source.dfg.display_inst(inst).to_string();
        let results = self.unit.source.dfg.inst_results(inst).to_vec();
        if results.len() == 1 {
            if let Some(alias) = self.facts.aliases.exprs.get(&results[0]).copied() {
                let value = self.expr_for_inst_data(alias);
                self.capture_value_from_expr(results[0], value, stmts);
                return Ok(());
            }
            if let Some(source) = direct_copy_source(&data) {
                if self.canonical_value(source) == self.canonical_value(results[0]) {
                    let value = self.expr_for_value(source);
                    self.capture_value_from_expr(results[0], value, stmts);
                    return Ok(());
                }
            }
        }
        if let InstructionData::Call { func_ref, args } = &data {
            return self.lower_call_inst(*func_ref, args.clone(), &results, &rendered, stmts);
        }
        match self.expr_for_inst(&data) {
            Some(expr) if results.len() == 1 => {
                let dst = self.local_for_result(results[0]);
                stmts.push(Stmt::Assign { dst, value: expr });
                self.capture_value_from_expr(results[0], Expr::Local(dst), stmts);
            }
            Some(expr) if results.is_empty() => {
                stmts.push(Stmt::Expr(expr));
            }
            Some(expr) => {
                bail!(
                    "unsupported multi-result MIR instruction in {}: {rendered} lowered to {expr:?}",
                    self.unit.name
                );
            }
            None => {
                bail!("unsupported MIR instruction in {}: {rendered}", self.unit.name);
            }
        }
        Ok(())
    }

    fn lower_call_inst(
        &mut self,
        func_ref: FuncRef,
        args: mir::ValueList,
        results: &[Value],
        rendered: &str,
        stmts: &mut Vec<Stmt>,
    ) -> Result<()> {
        let target = call_name(self.unit.source, func_ref);
        let args = args
            .as_slice(&self.unit.source.dfg.insts.value_lists)
            .iter()
            .map(|arg| self.expr_for_value(*arg))
            .collect::<Vec<_>>();

        let lowered = match self.unit.callbacks {
            Some(intern) => {
                if usize::from(func_ref) >= intern.callbacks.len() {
                    bail!(
                        "MIR call {target:?} in {} has no HIR callback metadata: {rendered}",
                        self.unit.name
                    );
                }
                classify_hir_callback(&intern.callbacks[func_ref], args)
            }
            None => classify_signature_call(&target, args),
        };

        match lowered {
            LoweredCall::Expr(expr) if results.len() == 1 => {
                let dst = self.local_for_result(results[0]);
                stmts.push(Stmt::Assign { dst, value: expr });
                self.capture_value_from_expr(results[0], Expr::Local(dst), stmts);
            }
            LoweredCall::Expr(_) => {
                bail!(
                    "MIR call {target:?} in {} must produce exactly one result: {rendered}",
                    self.unit.name
                );
            }
            LoweredCall::Effect(effect) if results.is_empty() => {
                stmts.push(Stmt::CallEffect(effect));
            }
            LoweredCall::Effect(_) => {
                bail!(
                    "MIR call {target:?} in {} has side-effect semantics but also returns values: {rendered}",
                    self.unit.name
                );
            }
            LoweredCall::Unsupported => {
                bail!(
                    "unsupported MIR call target in {}: {target:?} from {rendered}",
                    self.unit.name
                );
            }
        }
        Ok(())
    }

    fn capture_value_from_expr(&self, result: Value, value: Expr, stmts: &mut Vec<Stmt>) {
        if !self.unit.capture_values.contains(&result) {
            return;
        }
        stmts.push(Stmt::Capture { key: self.output_key_for_value(result), value });
    }

    fn expr_for_inst(&mut self, data: &InstructionData) -> Option<Expr> {
        match data {
            InstructionData::Unary { opcode, arg } => {
                let arg = Box::new(self.expr_for_value(*arg));
                unary_op(*opcode).map(|op| {
                    if *opcode == Opcode::OptBarrier {
                        *arg
                    } else {
                        Expr::Unary { op, arg }
                    }
                })
            }
            InstructionData::Binary { opcode, args } => binary_op(*opcode).map(|op| Expr::Binary {
                op,
                lhs: Box::new(self.expr_for_value(args[0])),
                rhs: Box::new(self.expr_for_value(args[1])),
            }),
            InstructionData::Call { .. } => None,
            _ => None,
        }
    }

    fn edge_label_or_block(&mut self, pred: Block, destination: Block) -> Label {
        let phis = self.phi_results(destination);
        if phis.is_empty() {
            return self.labels_by_block[&destination];
        }

        let label = self.fresh_label();
        let mut copies = Vec::with_capacity(phis.len());

        for result in phis {
            let Some(incoming) = self.phi_edge_value(result, pred) else {
                continue;
            };
            let dst = self.local_for_result(result);
            copies.push((result, incoming, dst));
        }

        let dsts = copies.iter().map(|(_, _, dst)| *dst).collect::<Vec<_>>();
        let needs_parallel_temps = copies.iter().any(|(_, incoming, _)| {
            if self.facts.aliases.exprs.contains_key(incoming) {
                return false;
            }
            match self.unit.source.dfg.value_def(self.canonical_value(*incoming)) {
                ValueDef::Param(_) | ValueDef::Result(_, _) => {
                    dsts.contains(&self.local_for_value(*incoming))
                }
                ValueDef::Const(_) | ValueDef::Invalid => false,
            }
        });

        let mut stmts =
            Vec::with_capacity(if needs_parallel_temps { copies.len() * 2 } else { copies.len() });
        if needs_parallel_temps {
            let mut temp_copies = Vec::with_capacity(copies.len());
            for (result, incoming, dst) in copies {
                let value = self.expr_for_value(incoming);
                let tmp = self.fresh_temp(
                    format!("phi_{}_from_{}", self.local_name_hint(result), block_name(pred)),
                    LirType::Unknown,
                    expr_variable_touched(&value, &self.local_variable_touched),
                );
                stmts.push(Stmt::Assign { dst: tmp, value });
                temp_copies.push((result, dst, tmp));
            }
            for (result, dst, tmp) in temp_copies {
                stmts.push(Stmt::Assign { dst, value: Expr::Local(tmp) });
                self.capture_value_from_expr(result, Expr::Local(dst), &mut stmts);
            }
        } else {
            for (result, incoming, dst) in copies {
                stmts.push(Stmt::Assign { dst, value: self.expr_for_value(incoming) });
                self.capture_value_from_expr(result, Expr::Local(dst), &mut stmts);
            }
        }

        self.blocks.push(crate::lir::Block {
            label,
            stmts,
            term: Terminator::Goto(self.labels_by_block[&destination]),
        });
        label
    }

    fn phi_results(&self, block: Block) -> Vec<Value> {
        self.unit
            .insts(block)
            .iter()
            .copied()
            .take_while(|inst| {
                matches!(self.unit.source.dfg.insts[*inst], InstructionData::PhiNode(_))
            })
            .filter_map(|inst| self.unit.source.dfg.inst_results(inst).first().copied())
            .collect()
    }

    fn phi_edge_value(&self, result: Value, pred: Block) -> Option<Value> {
        let ValueDef::Result(inst, 0) = self.unit.source.dfg.value_def(result) else {
            return None;
        };
        let InstructionData::PhiNode(phi) = &self.unit.source.dfg.insts[inst] else {
            return None;
        };
        self.unit.source.dfg.phi_edge_val(phi, pred)
    }

    fn return_values(&mut self) -> Vec<ReturnValue> {
        self.unit
            .return_values
            .iter()
            .map(|value| ReturnValue::Named {
                key: self.output_key_for_value(*value),
                value: self.expr_for_value(*value),
            })
            .collect()
    }

    fn output_key_for_value(&self, value: Value) -> String {
        self.unit.output_name_hints.get(&value).cloned().unwrap_or_else(|| value_name(value))
    }

    fn output_type_for_value(&self, value: Value) -> LirType {
        match self.unit.output_type_hints.get(&value).copied() {
            Some(LirType::Unknown) | None => {
                type_for_value(self.unit.source, self.canonical_value(value))
            }
            Some(ty) => ty,
        }
    }

    fn output_types(&self) -> HashMap<String, LirType> {
        self.unit
            .return_values
            .iter()
            .chain(self.unit.capture_values.iter())
            .map(|value| (self.output_key_for_value(*value), self.output_type_for_value(*value)))
            .fold(HashMap::new(), |mut types, (key, ty)| {
                merge_output_type_hint(&mut types, key, ty);
                types
            })
    }

    fn expr_for_value(&mut self, value: Value) -> Expr {
        if let Some(alias) = self.facts.aliases.exprs.get(&value).copied() {
            return self.expr_for_inst_data(alias);
        }
        let value = self.canonical_value(value);
        match self.unit.source.dfg.value_def(value) {
            ValueDef::Const(Const::Bool(value)) => Expr::Const(ConstValue::Bool(value)),
            ValueDef::Const(Const::Int(value)) => Expr::Const(ConstValue::Int(value)),
            ValueDef::Const(Const::Float(value)) => Expr::Const(ConstValue::Real(f64::from(value))),
            ValueDef::Const(Const::Str(value)) => {
                Expr::Const(ConstValue::Str(self.resolver.resolve(&value).to_owned()))
            }
            ValueDef::Invalid => Expr::Const(ConstValue::None),
            ValueDef::Param(_) | ValueDef::Result(_, _) => Expr::Local(self.local_for_value(value)),
        }
    }

    fn local_id_for_return(&mut self, value: Value) -> Option<LocalId> {
        let value = self.canonical_value(value);
        if self.facts.aliases.exprs.contains_key(&value) {
            return None;
        }
        match self.unit.source.dfg.value_def(value) {
            ValueDef::Param(_) | ValueDef::Result(_, _) => Some(self.local_for_value(value)),
            ValueDef::Const(_) | ValueDef::Invalid => None,
        }
    }

    fn local_for_result(&mut self, value: Value) -> LocalId {
        if self.unit.capture_values.contains(&value) {
            return self.local_for_exact_value(value);
        }
        self.local_for_value(value)
    }

    fn local_for_value(&mut self, value: Value) -> LocalId {
        let value = self.canonical_value(value);
        self.local_for_exact_value(value)
    }

    fn local_for_exact_value(&mut self, value: Value) -> LocalId {
        if let Some(&id) = self.locals_by_value.get(&value) {
            return id;
        }

        let id = LocalId(self.locals.len());
        let variable_touched = self.value_variable_touched(value);
        let generic_temp_name = self.value_has_generic_temp_name(value);
        self.locals.push(Local {
            id,
            name_hint: self.name_hint_for_value(value),
            ty: self
                .unit
                .output_type_hints
                .get(&value)
                .copied()
                .filter(|ty| *ty != LirType::Unknown)
                .unwrap_or_else(|| type_for_value(self.unit.source, value)),
        });
        self.locals_by_value.insert(value, id);
        self.local_variable_touched.insert(id, variable_touched);
        self.local_generic_temp_name.insert(id, generic_temp_name);
        id
    }

    fn name_hint_for_value(&self, value: Value) -> String {
        match self.unit.source.dfg.value_def(value) {
            ValueDef::Param(param) => {
                self.unit.param_name_hints.get(&param).cloned().unwrap_or_else(|| value_name(value))
            }
            ValueDef::Result(inst, 0) => {
                self.name_hint_for_call_result(inst).unwrap_or_else(|| value_name(value))
            }
            _ => value_name(value),
        }
    }

    fn name_hint_for_call_result(&self, inst: Inst) -> Option<String> {
        let InstructionData::Call { func_ref, args } = &self.unit.source.dfg.insts[inst] else {
            return None;
        };
        if !self.call_is_simparam_opt(*func_ref) {
            return None;
        }

        let name = args
            .as_slice(&self.unit.source.dfg.insts.value_lists)
            .first()
            .and_then(|value| self.string_const(*value))?;
        Some(format!("simparam_{name}"))
    }

    fn value_has_generic_temp_name(&self, value: Value) -> bool {
        match self.unit.source.dfg.value_def(value) {
            ValueDef::Param(param) => !self.unit.param_name_hints.contains_key(&param),
            ValueDef::Result(inst, 0) => self.name_hint_for_call_result(inst).is_none(),
            _ => true,
        }
    }

    fn call_is_simparam_opt(&self, func_ref: FuncRef) -> bool {
        match self.unit.callbacks {
            Some(intern) => {
                usize::from(func_ref) < intern.callbacks.len()
                    && intern.callbacks[func_ref] == CallBackKind::SimParamOpt
            }
            None => call_name(self.unit.source, func_ref) == "simparam_opt",
        }
    }

    fn string_const(&self, value: Value) -> Option<String> {
        match self.unit.source.dfg.value_def(self.canonical_value(value)) {
            ValueDef::Const(Const::Str(value)) => Some(self.resolver.resolve(&value).to_owned()),
            _ => None,
        }
    }

    fn canonical_value(&self, mut value: Value) -> Value {
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

    fn local_name_hint(&self, value: Value) -> String {
        value_name(self.canonical_value(value))
    }

    fn expr_for_inst_data(&mut self, data: AliasExpr) -> Expr {
        match data {
            AliasExpr::Unary { opcode, arg } => {
                let arg = Box::new(self.expr_for_value(arg));
                let op = unary_op(opcode).expect("aliasable unary opcode must lower");
                Expr::Unary { op, arg }
            }
            AliasExpr::Binary { opcode, args } => {
                let op = binary_op(opcode).expect("aliasable binary opcode must lower");
                Expr::Binary {
                    op,
                    lhs: Box::new(self.expr_for_value(args[0])),
                    rhs: Box::new(self.expr_for_value(args[1])),
                }
            }
        }
    }

    fn fresh_temp(&mut self, name_hint: String, ty: LirType, variable_touched: bool) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(Local { id, name_hint, ty });
        self.local_variable_touched.insert(id, variable_touched);
        self.local_generic_temp_name.insert(id, false);
        id
    }

    fn value_variable_touched(&mut self, value: Value) -> bool {
        if let Some(alias) = self.facts.aliases.exprs.get(&value).copied() {
            return self.alias_expr_variable_touched(alias);
        }

        let value = self.canonical_value(value);
        if let Some(&touched) = self.variable_touched_by_value.get(&value) {
            return touched;
        }
        if !self.variable_touched_in_progress.insert(value) {
            return true;
        }

        let touched = match self.unit.source.dfg.value_def(value) {
            ValueDef::Const(_) => false,
            ValueDef::Param(param) => !self.unit.param_name_hints.contains_key(&param),
            ValueDef::Result(inst, _) => self.inst_variable_touched(inst),
            ValueDef::Invalid => true,
        };

        self.variable_touched_in_progress.remove(&value);
        self.variable_touched_by_value.insert(value, touched);
        touched
    }

    fn inst_variable_touched(&mut self, inst: Inst) -> bool {
        match self.unit.source.dfg.insts[inst].clone() {
            InstructionData::Unary { opcode, arg } => {
                if unary_op(opcode).is_some() {
                    self.value_variable_touched(arg)
                } else {
                    true
                }
            }
            InstructionData::Binary { opcode, args } => {
                if binary_op(opcode).is_some() {
                    args.iter().any(|arg| self.value_variable_touched(*arg))
                } else {
                    true
                }
            }
            InstructionData::PhiNode(phi) => self
                .unit
                .blocks
                .iter()
                .filter_map(|pred| self.unit.source.dfg.phi_edge_val(&phi, *pred))
                .any(|incoming| self.value_variable_touched(incoming)),
            InstructionData::Call { .. } => true,
            InstructionData::Jump { .. }
            | InstructionData::Branch { .. }
            | InstructionData::Exit => true,
        }
    }

    fn alias_expr_variable_touched(&mut self, data: AliasExpr) -> bool {
        match data {
            AliasExpr::Unary { opcode, arg } => {
                if unary_op(opcode).is_some() {
                    self.value_variable_touched(arg)
                } else {
                    true
                }
            }
            AliasExpr::Binary { opcode, args } => {
                if binary_op(opcode).is_some() {
                    args.iter().any(|arg| self.value_variable_touched(*arg))
                } else {
                    true
                }
            }
        }
    }

    fn fresh_label(&self) -> Label {
        Label(self.labels_by_block.len() + self.blocks.len())
    }
}

fn unary_op(opcode: Opcode) -> Option<UnaryOp> {
    let op = match opcode {
        Opcode::Inot | Opcode::Bnot => UnaryOp::Not,
        Opcode::Fneg | Opcode::Ineg => UnaryOp::Neg,
        Opcode::FIcast | Opcode::BIcast => UnaryOp::Cast(LirType::Int),
        Opcode::IFcast | Opcode::BFcast => UnaryOp::Cast(LirType::Real),
        Opcode::IBcast | Opcode::FBcast => UnaryOp::Cast(LirType::Bool),
        Opcode::OptBarrier => UnaryOp::Cast(LirType::Unknown),
        Opcode::Sqrt => UnaryOp::Math1(MathUnary::Sqrt),
        Opcode::Exp => UnaryOp::Math1(MathUnary::Exp),
        Opcode::Ln => UnaryOp::Math1(MathUnary::Ln),
        Opcode::Log => UnaryOp::Math1(MathUnary::Log10),
        Opcode::Clog2 => UnaryOp::Math1(MathUnary::Clog2),
        Opcode::Floor => UnaryOp::Math1(MathUnary::Floor),
        Opcode::Ceil => UnaryOp::Math1(MathUnary::Ceil),
        Opcode::Sin => UnaryOp::Math1(MathUnary::Sin),
        Opcode::Cos => UnaryOp::Math1(MathUnary::Cos),
        Opcode::Tan => UnaryOp::Math1(MathUnary::Tan),
        Opcode::Asin => UnaryOp::Math1(MathUnary::Asin),
        Opcode::Acos => UnaryOp::Math1(MathUnary::Acos),
        Opcode::Atan => UnaryOp::Math1(MathUnary::Atan),
        Opcode::Sinh => UnaryOp::Math1(MathUnary::Sinh),
        Opcode::Cosh => UnaryOp::Math1(MathUnary::Cosh),
        Opcode::Tanh => UnaryOp::Math1(MathUnary::Tanh),
        Opcode::Asinh => UnaryOp::Math1(MathUnary::Asinh),
        Opcode::Acosh => UnaryOp::Math1(MathUnary::Acosh),
        Opcode::Atanh => UnaryOp::Math1(MathUnary::Atanh),
        _ => return None,
    };
    Some(op)
}

fn binary_op(opcode: Opcode) -> Option<BinaryOp> {
    let op = match opcode {
        Opcode::Iadd | Opcode::Fadd => BinaryOp::Add,
        Opcode::Isub | Opcode::Fsub => BinaryOp::Sub,
        Opcode::Imul | Opcode::Fmul => BinaryOp::Mul,
        Opcode::Idiv | Opcode::Fdiv => BinaryOp::Div,
        Opcode::Irem | Opcode::Frem => BinaryOp::Rem,
        Opcode::Ishl => BinaryOp::Shl,
        Opcode::Ishr => BinaryOp::Shr,
        Opcode::Ixor => BinaryOp::BitXor,
        Opcode::Iand => BinaryOp::BitAnd,
        Opcode::Ior => BinaryOp::BitOr,
        Opcode::Ilt | Opcode::Flt => BinaryOp::Lt,
        Opcode::Igt | Opcode::Fgt => BinaryOp::Gt,
        Opcode::Ige | Opcode::Fge => BinaryOp::Ge,
        Opcode::Ile | Opcode::Fle => BinaryOp::Le,
        Opcode::Ieq | Opcode::Feq | Opcode::Seq | Opcode::Beq => BinaryOp::Eq,
        Opcode::Ine | Opcode::Fne | Opcode::Sne | Opcode::Bne => BinaryOp::Ne,
        Opcode::Hypot => BinaryOp::Math2(MathBinary::Hypot),
        Opcode::Atan2 => BinaryOp::Math2(MathBinary::Atan2),
        Opcode::Pow => BinaryOp::Math2(MathBinary::Pow),
        _ => return None,
    };
    Some(op)
}

fn type_for_value(function: &Function, value: Value) -> LirType {
    match function.dfg.value_def(value) {
        ValueDef::Const(Const::Bool(_)) => LirType::Bool,
        ValueDef::Const(Const::Int(_)) => LirType::Int,
        ValueDef::Const(Const::Float(_)) => LirType::Real,
        ValueDef::Const(Const::Str(_)) => LirType::Str,
        ValueDef::Result(inst, _) => type_for_inst(&function.dfg.insts[inst]),
        ValueDef::Param(_) | ValueDef::Invalid => LirType::Unknown,
    }
}

fn type_for_inst(data: &InstructionData) -> LirType {
    match data {
        InstructionData::Unary { opcode, .. } => match opcode {
            Opcode::Inot | Opcode::Bnot | Opcode::IBcast | Opcode::FBcast => LirType::Bool,
            Opcode::FIcast | Opcode::BIcast => LirType::Int,
            Opcode::IFcast | Opcode::BFcast => LirType::Real,
            Opcode::Fneg
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
            | Opcode::Atanh => LirType::Real,
            Opcode::Ineg => LirType::Int,
            _ => LirType::Unknown,
        },
        InstructionData::Binary { opcode, .. } => match opcode {
            Opcode::Ilt
            | Opcode::Igt
            | Opcode::Ige
            | Opcode::Ile
            | Opcode::Flt
            | Opcode::Fgt
            | Opcode::Fge
            | Opcode::Fle
            | Opcode::Ieq
            | Opcode::Feq
            | Opcode::Seq
            | Opcode::Beq
            | Opcode::Ine
            | Opcode::Fne
            | Opcode::Sne
            | Opcode::Bne => LirType::Bool,
            Opcode::Fadd
            | Opcode::Fsub
            | Opcode::Fmul
            | Opcode::Fdiv
            | Opcode::Frem
            | Opcode::Hypot
            | Opcode::Atan2
            | Opcode::Pow => LirType::Real,
            Opcode::Iadd
            | Opcode::Isub
            | Opcode::Imul
            | Opcode::Idiv
            | Opcode::Irem
            | Opcode::Ishl
            | Opcode::Ishr
            | Opcode::Ixor
            | Opcode::Iand
            | Opcode::Ior => LirType::Int,
            _ => LirType::Unknown,
        },
        _ => LirType::Unknown,
    }
}

fn merge_output_type_hint(types: &mut HashMap<String, LirType>, key: String, ty: LirType) {
    types.entry(key).and_modify(|current| *current = merged_lir_type(*current, ty)).or_insert(ty);
}

fn merged_lir_type(lhs: LirType, rhs: LirType) -> LirType {
    use LirType::*;
    match (lhs, rhs) {
        (same_lhs, same_rhs) if same_lhs == same_rhs => same_lhs,
        (Unknown, ty) | (ty, Unknown) => ty,
        (Real, Int | Bool) | (Int | Bool, Real) => Real,
        (Int, Bool) | (Bool, Int) => Int,
        _ => Unknown,
    }
}

fn call_name(function: &Function, func_ref: FuncRef) -> String {
    let sig = &function.dfg.signatures[func_ref];
    if sig.name.is_empty() {
        format!("func_ref_{}", usize::from(func_ref))
    } else {
        sig.name.clone()
    }
}

#[derive(Debug)]
enum LoweredCall {
    Expr(Expr),
    Effect(CallEffect),
    Unsupported,
}

fn classify_hir_callback(callback: &CallBackKind, args: Vec<Expr>) -> LoweredCall {
    match callback {
        CallBackKind::SimParamOpt => {
            if args.len() == 2 {
                LoweredCall::Expr(Expr::SimparamOpt {
                    name: Box::new(args[0].clone()),
                    default: Box::new(args[1].clone()),
                })
            } else {
                LoweredCall::Unsupported
            }
        }
        CallBackKind::Print { kind, .. } => {
            LoweredCall::Effect(CallEffect::Diagnostic { target: format!("{kind:?}"), args })
        }
        CallBackKind::ParamInfo(ParamInfoKind::Invalid, param) => {
            LoweredCall::Effect(CallEffect::SetInvalidParam { param: format!("{param:?}") })
        }
        CallBackKind::CollapseHint(hi, lo) => LoweredCall::Effect(CallEffect::CollapseHint {
            hi: format!("{hi:?}"),
            lo: lo.map(|node| format!("{node:?}")),
        }),
        _ => LoweredCall::Unsupported,
    }
}

// Temporary adapter for MIR text that has lost HirInterner callback identity.
// OpenVAF production lowering passes structured CallBackKind above; this accepts only
// signature encodings produced by hir_lower::CallBackKind::signature plus the sanitized
// spelling produced by normalize_mir_input for legacy text dumps.
fn classify_signature_call(target: &str, args: Vec<Expr>) -> LoweredCall {
    if target == "simparam_opt" {
        if args.len() == 2 {
            return LoweredCall::Expr(Expr::SimparamOpt {
                name: Box::new(args[0].clone()),
                default: Box::new(args[1].clone()),
            });
        }
        return LoweredCall::Unsupported;
    }

    if diagnostic_target_kind(target).is_some() {
        return LoweredCall::Effect(CallEffect::Diagnostic { target: target.to_owned(), args });
    }

    if let Some(param) = parse_invalid_param_target(target) {
        return LoweredCall::Effect(CallEffect::SetInvalidParam { param });
    }

    if let Some((hi, lo)) = parse_collapse_hint_target(target) {
        return LoweredCall::Effect(CallEffect::CollapseHint { hi, lo });
    }

    LoweredCall::Unsupported
}

fn diagnostic_target_kind(target: &str) -> Option<&str> {
    const KINDS: [&str; 7] =
        ["Debug)", "Display)", "Info)", "Warn)", "Error)", "Fatal)", "Monitor)"];
    KINDS.iter().copied().find(|kind| *kind == target)
}

fn parse_invalid_param_target(target: &str) -> Option<String> {
    if let Some(raw) = target.strip_prefix("set_Invalid(").and_then(|rest| rest.strip_suffix(')')) {
        if raw.starts_with("Parameter { id: ParamId(") && raw.ends_with(") }") {
            return Some(raw.to_owned());
        }
    }

    let rest = target.strip_prefix("set_Invalid_Parameter___id__ParamId_")?;
    let id = rest.strip_suffix("____")?;
    if id.chars().all(|ch| ch.is_ascii_digit()) {
        Some(format!("Parameter {{ id: ParamId({id}) }}"))
    } else {
        None
    }
}

fn parse_collapse_hint_target(target: &str) -> Option<(String, Option<String>)> {
    let (hi_digits, rest) = split_leading_digits(target.strip_prefix("collapse_node")?)?;
    let hi = format!("node{hi_digits}");
    if rest == "_None" {
        return Some((hi, None));
    }
    let lo_digits = rest
        .strip_prefix("_Some(node")
        .and_then(|lo| lo.strip_suffix(')'))
        .or_else(|| rest.strip_prefix("_Some_node").and_then(|lo| lo.strip_suffix('_')))?;
    if lo_digits.chars().all(|ch| ch.is_ascii_digit()) {
        Some((hi, Some(format!("node{lo_digits}"))))
    } else {
        None
    }
}

fn split_leading_digits(input: &str) -> Option<(&str, &str)> {
    let len = input.chars().take_while(|ch| ch.is_ascii_digit()).map(char::len_utf8).sum();
    if len == 0 {
        None
    } else {
        Some(input.split_at(len))
    }
}

fn value_name(value: Value) -> String {
    format!("{value}").replace('.', "_")
}

fn block_name(block: Block) -> String {
    format!("{block}").replace('.', "_")
}

fn expr_variable_touched(expr: &Expr, local_variable_touched: &HashMap<LocalId, bool>) -> bool {
    match expr {
        Expr::Local(local) => local_variable_touched.get(local).copied().unwrap_or(true),
        Expr::Const(_) => false,
        Expr::Unary { arg, .. } | Expr::Abs { arg } => {
            expr_variable_touched(arg, local_variable_touched)
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Max { lhs, rhs } | Expr::Min { lhs, rhs } => {
            expr_variable_touched(lhs, local_variable_touched)
                || expr_variable_touched(rhs, local_variable_touched)
        }
        Expr::SimparamOpt { .. } | Expr::Call { .. } | Expr::Unsupported { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    use crate::lir::{CallEffect, ConstValue, Expr};

    use super::{classify_signature_call, LoweredCall};

    #[test]
    fn adapter_classifies_known_signature_call_targets() {
        match classify_signature_call(
            "simparam_opt",
            vec![
                Expr::Const(ConstValue::Str("gmin".to_owned())),
                Expr::Const(ConstValue::Real(1e-12)),
            ],
        ) {
            LoweredCall::Expr(Expr::SimparamOpt { default, .. }) => {
                assert_eq!(*default, Expr::Const(ConstValue::Real(1e-12)));
            }
            other => panic!("unexpected classification: {other:?}"),
        }

        match classify_signature_call(
            "Display)",
            vec![Expr::Const(ConstValue::Str("warn".to_owned()))],
        ) {
            LoweredCall::Effect(CallEffect::Diagnostic { target, args }) => {
                assert_eq!(target, "Display)");
                assert_eq!(args.len(), 1);
            }
            other => panic!("unexpected classification: {other:?}"),
        }

        match classify_signature_call("set_Invalid(Parameter { id: ParamId(12) })", Vec::new()) {
            LoweredCall::Effect(CallEffect::SetInvalidParam { param }) => {
                assert_eq!(param, "Parameter { id: ParamId(12) }")
            }
            other => panic!("unexpected classification: {other:?}"),
        }

        match classify_signature_call("collapse_node3_Some(node10)", Vec::new()) {
            LoweredCall::Effect(CallEffect::CollapseHint { hi, lo }) => {
                assert_eq!(hi, "node3");
                assert_eq!(lo.as_deref(), Some("node10"));
            }
            other => panic!("unexpected classification: {other:?}"),
        }

        match classify_signature_call("collapse_node3_Some_node10_", Vec::new()) {
            LoweredCall::Effect(CallEffect::CollapseHint { hi, lo }) => {
                assert_eq!(hi, "node3");
                assert_eq!(lo.as_deref(), Some("node10"));
            }
            other => panic!("unexpected classification: {other:?}"),
        }

        match classify_signature_call("collapse_node2_None", Vec::new()) {
            LoweredCall::Effect(CallEffect::CollapseHint { hi, lo }) => {
                assert_eq!(hi, "node2");
                assert_eq!(lo, None);
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn adapter_accepts_normalized_invalid_param_name() {
        match classify_signature_call("set_Invalid_Parameter___id__ParamId_7____", Vec::new()) {
            LoweredCall::Effect(CallEffect::SetInvalidParam { param }) => {
                assert_eq!(param, "Parameter { id: ParamId(7) }")
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn adapter_rejects_unknown_or_malformed_signature_call_targets() {
        assert!(matches!(
            classify_signature_call("not_implemented", Vec::new()),
            LoweredCall::Unsupported
        ));
        assert!(matches!(
            classify_signature_call("set_Invalid(Parameter { id: Missing })", Vec::new()),
            LoweredCall::Unsupported
        ));
        assert!(matches!(
            classify_signature_call("collapse_node_x_Some(node1)", Vec::new()),
            LoweredCall::Unsupported
        ));
    }
}
