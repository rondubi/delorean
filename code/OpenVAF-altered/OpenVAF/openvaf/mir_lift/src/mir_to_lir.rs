use std::collections::HashMap;

use anyhow::Result;
use lasso::Resolver;
use mir::{Block, Const, FuncRef, Function, Inst, InstructionData, Opcode, Value, ValueDef};

use crate::lir::{
    BinaryOp, ConstValue, Expr, Label, LirType, Local, LocalId, MathBinary, MathUnary, ReturnSlot,
    ReturnValue, Stmt, Terminator, UnaryOp,
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
                    self.local_for_value(result);
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
                self.local_id_for_return(*value)
                    .map(|local| ReturnSlot { key: value_name(*value), value: local })
            })
            .collect();

        for &block in &self.unit.blocks {
            self.lower_block(block);
        }

        Ok(crate::lir::Function {
            name: self.unit.name.clone(),
            params,
            locals: self.locals.clone(),
            entry: self.labels_by_block[&self.unit.entry],
            blocks: self.blocks.clone(),
            returns,
        })
    }

    fn lower_block(&mut self, block: Block) {
        let mut stmts = Vec::new();
        for &inst in self.unit.insts(block) {
            let data = self.unit.source.dfg.insts[inst].clone();
            if data.is_phi() || data.is_terminator() {
                continue;
            }
            self.lower_inst(inst, data, &mut stmts);
        }

        let term = match self
            .unit
            .source
            .layout
            .block_terminator(block)
            .map(|inst| self.unit.source.dfg.insts[inst].clone())
        {
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
            Some(InstructionData::Exit) | None => Terminator::Return(self.return_values()),
            Some(other) => {
                stmts.push(Stmt::Unsupported { dsts: Vec::new(), text: format!("{other:?}") });
                Terminator::Return(self.return_values())
            }
        };

        self.blocks.push(crate::lir::Block { label: self.labels_by_block[&block], stmts, term });
    }

    fn lower_inst(&mut self, inst: Inst, data: InstructionData, stmts: &mut Vec<Stmt>) {
        let rendered = self.unit.source.dfg.display_inst(inst).to_string();
        let results = self.unit.source.dfg.inst_results(inst).to_vec();
        if results.len() == 1 {
            if self.facts.aliases.exprs.contains_key(&results[0]) {
                return;
            }
            if let Some(source) = direct_copy_source(&data) {
                if self.canonical_value(source) == self.canonical_value(results[0]) {
                    return;
                }
            }
        }
        match self.expr_for_inst(&data) {
            Some(expr) if results.len() == 1 => {
                let dst = self.local_for_value(results[0]);
                stmts.push(Stmt::Assign { dst, value: expr });
            }
            Some(expr) if results.is_empty() => {
                stmts.push(Stmt::Expr(expr));
            }
            Some(expr) => {
                let dsts = results.iter().map(|value| self.local_for_value(*value)).collect();
                stmts.push(Stmt::Unsupported {
                    dsts,
                    text: format!("{rendered} lowered to multi-result expression {expr:?}"),
                });
            }
            None => {
                let dsts = results.iter().map(|value| self.local_for_value(*value)).collect();
                stmts.push(Stmt::Unsupported { dsts, text: rendered });
            }
        }
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
            InstructionData::Call { func_ref, args } => Some(Expr::Call {
                target: call_name(self.unit.source, *func_ref),
                args: args
                    .as_slice(&self.unit.source.dfg.insts.value_lists)
                    .iter()
                    .map(|arg| self.expr_for_value(*arg))
                    .collect(),
            }),
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
            let dst = self.local_for_value(result);
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
                let tmp = self.fresh_temp(
                    format!("phi_{}_from_{}", self.local_name_hint(result), block_name(pred)),
                    LirType::Unknown,
                );
                stmts.push(Stmt::Assign { dst: tmp, value: self.expr_for_value(incoming) });
                temp_copies.push((dst, tmp));
            }
            for (dst, tmp) in temp_copies {
                stmts.push(Stmt::Assign { dst, value: Expr::Local(tmp) });
            }
        } else {
            for (_, incoming, dst) in copies {
                stmts.push(Stmt::Assign { dst, value: self.expr_for_value(incoming) });
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
                key: value_name(*value),
                value: self.expr_for_value(*value),
            })
            .collect()
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

    fn local_for_value(&mut self, value: Value) -> LocalId {
        let value = self.canonical_value(value);
        if let Some(&id) = self.locals_by_value.get(&value) {
            return id;
        }

        let id = LocalId(self.locals.len());
        self.locals.push(Local {
            id,
            name_hint: value_name(value),
            ty: type_for_value(self.unit.source, value),
        });
        self.locals_by_value.insert(value, id);
        id
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

    fn fresh_temp(&mut self, name_hint: String, ty: LirType) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(Local { id, name_hint, ty });
        id
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

fn call_name(function: &Function, func_ref: FuncRef) -> String {
    let sig = &function.dfg.signatures[func_ref];
    if sig.name.is_empty() {
        format!("func_ref_{}", usize::from(func_ref))
    } else {
        sig.name.clone()
    }
}

fn value_name(value: Value) -> String {
    format!("{value}").replace('.', "_")
}

fn block_name(block: Block) -> String {
    format!("{block}").replace('.', "_")
}
