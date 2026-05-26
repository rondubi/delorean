#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use hir_lower::HirInterner;
use lasso::Resolver;
use mir::{
    Block, Const, ControlFlowGraph, FuncRef, Function, Inst, InstructionData, Opcode, Param, Value,
    ValueDef,
};
use mir_reader::parse_functions;
use serde::Deserialize;

pub mod lir;

mod lir_backward;
mod lir_forward;
mod lir_simplify;
mod lir_structure;
mod lir_to_python;
mod mir_forward;
mod mir_to_lir;

pub fn lift_text(input: &str, metadata_json: Option<&str>) -> Result<String> {
    lift_text_with_options(input, metadata_json, LiftOptions::default())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LiftOptions {
    pub dump_lir: bool,
}

pub fn lift_text_with_options(
    input: &str,
    metadata_json: Option<&str>,
    options: LiftOptions,
) -> Result<String> {
    let normalized = normalize_mir_input(input);
    let (functions, interner) = parse_functions(&normalized).map_err(|err| anyhow!("{err}"))?;
    let metadata = metadata_json
        .map(serde_json::from_str::<CompilationDb>)
        .transpose()
        .context("failed to parse compilation metadata JSON")?
        .unwrap_or_default();
    let units = build_units(&functions, &metadata)?;

    let mut out = String::new();
    emit_python_prelude(&mut out);

    for (index, unit) in units.iter().enumerate() {
        if index != 0 {
            out.push('\n');
            out.push('\n');
        }
        let resolver: &dyn Resolver = &interner;
        emit_lir_unit(&mut out, unit, resolver, options)?;
    }

    Ok(out)
}

pub fn dump_lir_text(input: &str, metadata_json: Option<&str>) -> Result<String> {
    let normalized = normalize_mir_input(input);
    let (functions, interner) = parse_functions(&normalized).map_err(|err| anyhow!("{err}"))?;
    let metadata = metadata_json
        .map(serde_json::from_str::<CompilationDb>)
        .transpose()
        .context("failed to parse compilation metadata JSON")?
        .unwrap_or_default();
    let units = build_units(&functions, &metadata)?;

    let mut out = String::new();
    for (index, unit) in units.iter().enumerate() {
        if index != 0 {
            out.push('\n');
        }
        let resolver: &dyn Resolver = &interner;
        let lir = lower_simplified_lir(unit, resolver)?;
        write!(&mut out, "{lir}")?;
    }
    Ok(out)
}

pub fn lift_function(function: &Function, resolver: &dyn Resolver) -> Result<String> {
    let unit = FunctionUnit::whole(function)?;
    emit_function_unit(&unit, resolver)
}

pub fn lift_function_with_returns(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_returns(function, return_values)?;
    emit_function_unit(&unit, resolver)
}

pub fn lift_function_with_hir_returns(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_and_param_hints(
        function,
        return_values,
        intern,
        HashMap::new(),
    )?;
    emit_function_unit(&unit, resolver)
}

pub fn lift_function_with_hir_returns_and_param_hints(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_and_param_hints(
        function,
        return_values,
        intern,
        param_name_hints,
    )?;
    emit_function_unit(&unit, resolver)
}

pub fn lift_function_with_returns_and_captures(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
) -> Result<String> {
    let unit = FunctionUnit::whole_named_with_returns_and_captures(
        function,
        sanitize_ident(&function.name),
        return_values,
        capture_values,
    )?;
    emit_function_unit(&unit, resolver)
}

pub fn lift_function_with_hir_returns_and_captures(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_captures_and_param_hints(
        function,
        return_values,
        capture_values,
        intern,
        HashMap::new(),
    )?;
    emit_function_unit(&unit, resolver)
}

pub fn lift_function_with_hir_returns_captures_and_param_hints(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_captures_and_param_hints(
        function,
        return_values,
        capture_values,
        intern,
        param_name_hints,
    )?;
    emit_function_unit(&unit, resolver)
}

pub fn dump_function_lir_with_returns(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_returns(function, return_values)?;
    let lir = lower_simplified_lir(&unit, resolver)?;
    Ok(lir.to_string())
}

pub fn dump_function_lir_with_hir_returns(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_and_param_hints(
        function,
        return_values,
        intern,
        HashMap::new(),
    )?;
    let lir = lower_simplified_lir(&unit, resolver)?;
    Ok(lir.to_string())
}

pub fn dump_function_lir_with_hir_returns_and_param_hints(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_and_param_hints(
        function,
        return_values,
        intern,
        param_name_hints,
    )?;
    let lir = lower_simplified_lir(&unit, resolver)?;
    Ok(lir.to_string())
}

pub fn dump_function_lir_with_returns_and_captures(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
) -> Result<String> {
    let unit = FunctionUnit::whole_named_with_returns_and_captures(
        function,
        sanitize_ident(&function.name),
        return_values,
        capture_values,
    )?;
    let lir = lower_simplified_lir(&unit, resolver)?;
    Ok(lir.to_string())
}

pub fn dump_function_lir_with_hir_returns_and_captures(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_captures_and_param_hints(
        function,
        return_values,
        capture_values,
        intern,
        HashMap::new(),
    )?;
    let lir = lower_simplified_lir(&unit, resolver)?;
    Ok(lir.to_string())
}

pub fn dump_function_lir_with_hir_returns_captures_and_param_hints(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_captures_and_param_hints(
        function,
        return_values,
        capture_values,
        intern,
        param_name_hints,
    )?;
    let lir = lower_simplified_lir(&unit, resolver)?;
    Ok(lir.to_string())
}

pub fn collect_live_param_refs_forward(function: &Function) -> Result<Vec<Param>> {
    let unit = FunctionUnit::whole(function)?;
    Ok(mir_forward::collect_live_param_refs(&unit))
}

fn emit_function_unit(unit: &FunctionUnit<'_>, resolver: &dyn Resolver) -> Result<String> {
    let mut out = String::new();
    emit_python_prelude(&mut out);
    emit_lir_unit(&mut out, unit, resolver, LiftOptions::default())?;
    Ok(out)
}

fn emit_lir_unit(
    out: &mut String,
    unit: &FunctionUnit<'_>,
    resolver: &dyn Resolver,
    options: LiftOptions,
) -> Result<()> {
    let timing = timing_enabled();
    let start = Instant::now();
    let lir = lower_simplified_lir(unit, resolver)?;
    log_timing(timing, &unit.name, "lower+simplify", start);
    if options.dump_lir {
        eprintln!("{lir}");
    }
    let start = Instant::now();
    out.push_str(&lir_to_python::emit_function(&lir)?);
    log_timing(timing, &unit.name, "emit-python", start);
    Ok(())
}

fn lower_simplified_lir(unit: &FunctionUnit<'_>, resolver: &dyn Resolver) -> Result<lir::Function> {
    let timing = timing_enabled();
    let start = Instant::now();
    let lir = mir_to_lir::lower_unit(unit, resolver)?;
    log_timing(timing, &unit.name, "mir-to-lir", start);
    let start = Instant::now();
    let lir = lir_simplify::simplify(lir);
    log_timing(timing, &unit.name, "lir-simplify", start);
    Ok(lir)
}

fn timing_enabled() -> bool {
    std::env::var_os("MIR_LIFT_TIMING").is_some()
}

fn log_timing(enabled: bool, function: &str, stage: &str, start: Instant) {
    if enabled {
        eprintln!("mir-lift timing: {function} {stage} {:?}", start.elapsed());
    }
}

fn emit_python_prelude(out: &mut String) {
    out.push_str("import math\n\n\n");
    out.push_str("inf = math.inf\n\n\n");
}

fn normalize_mir_input(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        let line = normalize_signature_decl(line);
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('@') {
            let hex_len =
                rest.chars().take_while(|ch| ch.is_ascii_hexdigit() || *ch == '-').count();
            if hex_len > 0 {
                let rest = &rest[hex_len..];
                let rest = rest.trim_start();
                let prefix_len = line.len() - trimmed.len();
                out.push_str(&line[..prefix_len]);
                out.push_str(rest);
                out.push('\n');
                continue;
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn normalize_signature_decl(line: &str) -> String {
    let mut line = line.to_owned();

    for marker in [" = fn ", " = const fn "] {
        if let Some(eq_pos) = line.find(marker) {
            if let Some(prefix) = line[..eq_pos].trim().strip_prefix("inst") {
                if prefix.chars().all(|ch| ch.is_ascii_digit()) {
                    let left_trimmed = line[..eq_pos].trim();
                    if let Some(start) = line.find(left_trimmed) {
                        let end = start + left_trimmed.len();
                        line.replace_range(start..end, &format!("fn{prefix}"));
                    }
                }
            }
        }
    }

    if let Some(call_pos) = line.find("call inst") {
        let digits_start = call_pos + "call inst".len();
        let digits_len = line[digits_start..].chars().take_while(|ch| ch.is_ascii_digit()).count();
        if digits_len > 0 {
            line.replace_range(call_pos + 5..call_pos + 9, "fn");
        }
    }

    let Some(fn_pos) = line.find("fn %") else {
        return line;
    };
    let name_start = fn_pos + 4;
    let Some(arrow_pos) = line.find("->") else {
        return line;
    };
    let Some(rel_name_end) = line[name_start..arrow_pos].rfind('(') else {
        return line;
    };
    let name_end = name_start + rel_name_end;
    if name_end <= name_start {
        return line;
    }

    let name = &line[name_start..name_end];
    if name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return line;
    }

    let sanitized = name
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' { ch } else { '_' })
        .collect::<String>();

    let mut out = String::with_capacity(line.len());
    out.push_str(&line[..name_start]);
    out.push_str(&sanitized);
    out.push_str(&line[name_end..]);
    out
}

#[derive(Debug, Default, Deserialize)]
struct CompilationDb {
    #[serde(default)]
    functions: Vec<FunctionMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
struct FunctionMetadata {
    name: String,
    #[serde(default)]
    mir_name: Option<String>,
    #[serde(default)]
    blocks: Vec<String>,
}

struct FunctionUnit<'a> {
    name: String,
    source: &'a Function,
    blocks: Vec<Block>,
    block_set: HashSet<Block>,
    preds_by_block: HashMap<Block, Vec<Block>>,
    insts_by_block: HashMap<Block, Vec<Inst>>,
    entry: Block,
    params: Vec<Value>,
    return_values: Vec<Value>,
    capture_values: Vec<Value>,
    callbacks: Option<&'a HirInterner>,
    param_name_hints: HashMap<Param, String>,
}

struct BlockGroup {
    id: usize,
    blocks: Vec<Block>,
}

#[derive(Clone, Copy)]
struct ExprAlias {
    opcode: Opcode,
    arg: Value,
}

fn build_units<'a>(
    functions: &'a [Function],
    metadata: &CompilationDb,
) -> Result<Vec<FunctionUnit<'a>>> {
    if metadata.functions.is_empty() {
        let mut units = Vec::with_capacity(functions.len());
        let mut used_names = HashSet::new();
        for (index, function) in functions.iter().enumerate() {
            let mut unit = FunctionUnit::whole(function)?;
            unit.name = unique_name(&unit.name, index, &mut used_names);
            units.push(unit);
        }
        return Ok(units);
    }

    let mut units = Vec::new();
    let mut used_names = HashSet::new();
    for spec in &metadata.functions {
        let source = match &spec.mir_name {
            Some(name) => functions
                .iter()
                .find(|func| func.name == *name)
                .with_context(|| format!("metadata references missing MIR function {:?}", name))?,
            None if functions.len() == 1 => &functions[0],
            None => bail!("metadata entry {:?} must include mir_name when input contains multiple MIR functions", spec.name),
        };

        if spec.blocks.is_empty() {
            let mut unit = FunctionUnit::whole_named(source, spec.name.clone())?;
            unit.name = unique_name(&unit.name, units.len(), &mut used_names);
            units.push(unit);
        } else {
            let mut unit = FunctionUnit::slice(source, spec)?;
            unit.name = unique_name(&unit.name, units.len(), &mut used_names);
            units.push(unit);
        }
    }

    Ok(units)
}

impl<'a> FunctionUnit<'a> {
    fn whole(source: &'a Function) -> Result<Self> {
        Self::whole_named(source, sanitize_ident(&source.name))
    }

    fn whole_with_returns(source: &'a Function, return_values: &[Value]) -> Result<Self> {
        Self::whole_named_with_returns(source, sanitize_ident(&source.name), return_values)
    }

    fn whole_named(source: &'a Function, name: String) -> Result<Self> {
        let blocks: Vec<Block> = source.layout.blocks().collect();
        let entry = source
            .layout
            .entry_block()
            .with_context(|| format!("function {} has no entry block", source.name))?;
        let block_set = blocks.iter().copied().collect::<HashSet<_>>();
        let insts_by_block = build_insts_by_block(source, &blocks);
        let preds_by_block = build_preds_by_block(source, &blocks, &block_set);
        let params = collect_function_params(source);
        let return_values = collect_return_values(source, &blocks, &insts_by_block, &params);
        Ok(Self {
            name,
            source,
            blocks,
            block_set,
            preds_by_block,
            insts_by_block,
            entry,
            params,
            return_values,
            capture_values: Vec::new(),
            callbacks: None,
            param_name_hints: HashMap::new(),
        })
    }

    fn whole_named_with_returns(
        source: &'a Function,
        name: String,
        return_values: &[Value],
    ) -> Result<Self> {
        Self::whole_named_with_returns_and_captures(source, name, return_values, &[])
    }

    fn whole_named_with_returns_and_captures(
        source: &'a Function,
        name: String,
        return_values: &[Value],
        capture_values: &[Value],
    ) -> Result<Self> {
        let blocks: Vec<Block> = source.layout.blocks().collect();
        let entry = source
            .layout
            .entry_block()
            .with_context(|| format!("function {} has no entry block", source.name))?;
        let block_set = blocks.iter().copied().collect::<HashSet<_>>();
        let insts_by_block = build_insts_by_block(source, &blocks);
        let preds_by_block = build_preds_by_block(source, &blocks, &block_set);
        let params = collect_function_params(source);
        let return_values = live_return_values(source, &blocks, return_values);
        let capture_values = live_return_values(source, &blocks, capture_values);
        Ok(Self {
            name,
            source,
            blocks,
            block_set,
            preds_by_block,
            insts_by_block,
            entry,
            params,
            return_values,
            capture_values,
            callbacks: None,
            param_name_hints: HashMap::new(),
        })
    }

    fn whole_with_hir_returns_and_param_hints(
        source: &'a Function,
        return_values: &[Value],
        callbacks: &'a HirInterner,
        param_name_hints: HashMap<Param, String>,
    ) -> Result<Self> {
        let mut unit = Self::whole_with_returns(source, return_values)?;
        unit.callbacks = Some(callbacks);
        unit.param_name_hints = param_name_hints;
        Ok(unit)
    }

    fn whole_with_hir_returns_captures_and_param_hints(
        source: &'a Function,
        return_values: &[Value],
        capture_values: &[Value],
        callbacks: &'a HirInterner,
        param_name_hints: HashMap<Param, String>,
    ) -> Result<Self> {
        let mut unit = Self::whole_named_with_returns_and_captures(
            source,
            sanitize_ident(&source.name),
            return_values,
            capture_values,
        )?;
        unit.callbacks = Some(callbacks);
        unit.param_name_hints = param_name_hints;
        Ok(unit)
    }

    fn slice(source: &'a Function, spec: &FunctionMetadata) -> Result<Self> {
        let mut named_blocks = HashMap::new();
        for block in source.layout.blocks() {
            named_blocks.insert(format!("{block}"), block);
        }

        let mut blocks = Vec::new();
        for raw in &spec.blocks {
            let block = named_blocks.get(raw).copied().with_context(|| {
                format!("metadata references unknown block {raw} in MIR function {}", source.name)
            })?;
            blocks.push(block);
        }

        let block_set = blocks.iter().copied().collect::<HashSet<_>>();
        let entry = *blocks
            .first()
            .with_context(|| format!("metadata function {} contained no blocks", spec.name))?;
        let insts_by_block = build_insts_by_block(source, &blocks);
        let preds_by_block = build_preds_by_block(source, &blocks, &block_set);
        let params = collect_slice_params(source, &block_set);
        let return_values = collect_return_values(source, &blocks, &insts_by_block, &params);

        Ok(Self {
            name: sanitize_ident(&spec.name),
            source,
            blocks,
            block_set,
            preds_by_block,
            insts_by_block,
            entry,
            params,
            return_values,
            capture_values: Vec::new(),
            callbacks: None,
            param_name_hints: HashMap::new(),
        })
    }

    fn contains_block(&self, block: Block) -> bool {
        self.block_set.contains(&block)
    }

    fn preds(&self, block: Block) -> &[Block] {
        self.preds_by_block.get(&block).map(Vec::as_slice).unwrap_or(&[])
    }

    fn insts(&self, block: Block) -> &[Inst] {
        self.insts_by_block.get(&block).map(Vec::as_slice).unwrap_or(&[])
    }
}

fn dedup_values(values: &[Value]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(values.len());
    for &value in values {
        if seen.insert(value) {
            out.push(value);
        }
    }
    out
}

fn live_return_values(function: &Function, blocks: &[Block], values: &[Value]) -> Vec<Value> {
    let insts =
        blocks.iter().flat_map(|block| function.layout.block_insts(*block)).collect::<HashSet<_>>();
    let values = values.iter().copied().filter(|value| match function.dfg.value_def(*value) {
        ValueDef::Param(_) | ValueDef::Const(_) => true,
        ValueDef::Result(inst, _) => insts.contains(&inst),
        ValueDef::Invalid => false,
    });
    dedup_values(&values.collect::<Vec<_>>())
}

fn build_insts_by_block(function: &Function, blocks: &[Block]) -> HashMap<Block, Vec<Inst>> {
    blocks
        .iter()
        .copied()
        .map(|block| (block, function.layout.block_insts(block).collect()))
        .collect()
}

fn build_preds_by_block(
    function: &Function,
    blocks: &[Block],
    block_set: &HashSet<Block>,
) -> HashMap<Block, Vec<Block>> {
    let cfg = ControlFlowGraph::with_function(function);
    blocks
        .iter()
        .copied()
        .map(|block| {
            let preds = cfg.pred_iter(block).filter(|pred| block_set.contains(pred)).collect();
            (block, preds)
        })
        .collect()
}

fn collect_return_values(
    function: &Function,
    blocks: &[Block],
    insts_by_block: &HashMap<Block, Vec<Inst>>,
    params: &[Value],
) -> Vec<Value> {
    let mut values = params.to_vec();
    let mut seen: HashSet<Value> = params.iter().copied().collect();

    for &block in blocks {
        if let Some(insts) = insts_by_block.get(&block) {
            for &inst in insts {
                for &result in function.dfg.inst_results(inst) {
                    if seen.insert(result) {
                        values.push(result);
                    }
                }
            }
        }
    }

    values
}

fn collect_function_params(function: &Function) -> Vec<Value> {
    let mut params: Vec<(usize, Value)> = function
        .dfg
        .values()
        .filter_map(|val| match function.dfg.value_def(val) {
            ValueDef::Param(param) => Some((param_index(param), val)),
            _ => None,
        })
        .collect();
    params.sort_by_key(|(index, _)| *index);
    params.into_iter().map(|(_, val)| val).collect()
}

fn collect_slice_params(function: &Function, blocks: &HashSet<Block>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut params = Vec::new();

    for block in function.layout.blocks() {
        if !blocks.contains(&block) {
            continue;
        }
        for inst in function.layout.block_insts(block) {
            for &arg in function.dfg.insts[inst].arguments(&function.dfg.insts.value_lists) {
                let external = match function.dfg.value_def(arg) {
                    ValueDef::Param(_) => true,
                    ValueDef::Const(_) => false,
                    ValueDef::Result(def_inst, _) => function
                        .layout
                        .inst_block(def_inst)
                        .map_or(true, |def_block| !blocks.contains(&def_block)),
                    ValueDef::Invalid => false,
                };
                if external && seen.insert(arg) {
                    params.push(arg);
                }
            }
        }
    }

    params.sort_by_key(|value| format!("{value}"));
    params
}

fn emit_unit(out: &mut String, unit: &FunctionUnit<'_>, resolver: &dyn Resolver) -> Result<()> {
    writeln!(
        out,
        "def {}({}):",
        unit.name,
        unit.params.iter().map(|value| value_name(*value)).collect::<Vec<_>>().join(", ")
    )?;

    emit_grouped_body(out, unit, resolver)?;

    Ok(())
}

fn emit_grouped_body(
    out: &mut String,
    unit: &FunctionUnit<'_>,
    resolver: &dyn Resolver,
) -> Result<()> {
    let groups = build_block_groups(unit);
    let groups_by_id: Vec<&BlockGroup> = groups.iter().collect();
    let group_by_block: HashMap<Block, usize> = groups
        .iter()
        .flat_map(|group| group.blocks.iter().copied().map(move |block| (block, group.id)))
        .collect();
    let inlineable = compute_inlineable_groups(unit, &groups, &group_by_block);
    let copy_aliases = compute_copy_aliases(unit);
    let expr_aliases = compute_expr_aliases(unit);
    let cross_group_values =
        cross_group_materialized_values(unit, &group_by_block, &copy_aliases, &expr_aliases);
    let localized_roots =
        localized_return_roots(unit, &cross_group_values, &copy_aliases, &expr_aliases);
    let state_values = shared_state_values(&cross_group_values);

    if !localized_roots.is_empty() {
        writeln!(out, "    _ret = {{}}")?;
    }

    for value in state_values
        .iter()
        .copied()
        .filter(|value| !matches!(unit.source.dfg.value_def(*value), ValueDef::Param(_)))
    {
        writeln!(out, "    {} = None", value_name(value))?;
    }
    if !state_values.is_empty() || !localized_roots.is_empty() {
        writeln!(out)?;
    }

    for group in &groups {
        if inlineable.contains(&group.id) {
            continue;
        }
        emit_group_function(
            out,
            unit,
            resolver,
            group,
            &groups_by_id,
            &group_by_block,
            &inlineable,
            &state_values,
            &copy_aliases,
            &expr_aliases,
            &localized_roots,
        )?;
        writeln!(out)?;
    }

    let entry_group = group_by_block[&unit.entry];
    let entry_args = group_phi_results(unit, unit.entry)
        .into_iter()
        .map(|value| phi_param_name(value))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "    _fn_{entry_group}({entry_args})")?;
    emit_return_values(
        out,
        unit.source,
        &unit.return_values,
        &copy_aliases,
        &expr_aliases,
        &localized_roots,
        resolver,
        1,
    )?;
    Ok(())
}

fn build_block_groups(unit: &FunctionUnit<'_>) -> Vec<BlockGroup> {
    fn leader(unit: &FunctionUnit<'_>, block: Block, memo: &mut HashMap<Block, Block>) -> Block {
        if let Some(&existing) = memo.get(&block) {
            return existing;
        }

        let leader = if block == unit.entry {
            block
        } else {
            let preds = unit.preds(block);
            if preds.len() == 1 {
                leader(unit, preds[0], memo)
            } else {
                block
            }
        };
        memo.insert(block, leader);
        leader
    }

    let mut memo = HashMap::new();
    let mut ids_by_leader = HashMap::new();
    let mut groups = Vec::new();

    for &block in &unit.blocks {
        let leader = leader(unit, block, &mut memo);
        let id = match ids_by_leader.get(&leader) {
            Some(&id) => id,
            None => {
                let id = groups.len();
                ids_by_leader.insert(leader, id);
                groups.push(BlockGroup { id, blocks: Vec::new() });
                id
            }
        };
        groups[id].blocks.push(block);
    }

    groups
}

fn emit_group_function(
    out: &mut String,
    unit: &FunctionUnit<'_>,
    resolver: &dyn Resolver,
    group: &BlockGroup,
    groups_by_id: &[&BlockGroup],
    group_by_block: &HashMap<Block, usize>,
    inlineable: &HashSet<usize>,
    state_values: &[Value],
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
) -> Result<()> {
    let state_set: HashSet<Value> = state_values.iter().copied().collect();
    let phi_results = group_phi_results(unit, group.blocks[0]);
    let params =
        phi_results.iter().map(|value| phi_param_name(*value)).collect::<Vec<_>>().join(", ");
    writeln!(out, "    def _fn_{}({params}):", group.id)?;
    let nonlocals =
        group_nonlocals(unit, group, groups_by_id, group_by_block, inlineable, state_values);
    if !nonlocals.is_empty() {
        writeln!(
            out,
            "        nonlocal {}",
            nonlocals.iter().map(|value| value_name(*value)).collect::<Vec<_>>().join(", ")
        )?;
    }
    let mut aliases = HashMap::new();
    for &value in &phi_results {
        let param = phi_param_name(value);
        if state_set.contains(&value) {
            writeln!(out, "        {} = {}", value_name(value), param)?;
            maybe_store_localized_root(
                out,
                value,
                &value_name(value),
                copy_aliases,
                localized_roots,
                2,
            )?;
        } else {
            aliases.insert(value, param);
            maybe_store_localized_alias(
                out,
                value,
                copy_aliases,
                expr_aliases,
                &aliases,
                localized_roots,
                2,
            )?;
        }
    }

    let mut emitted = HashSet::new();
    emit_group_block(
        out,
        unit,
        resolver,
        group,
        groups_by_id,
        group_by_block,
        inlineable,
        &state_set,
        copy_aliases,
        expr_aliases,
        localized_roots,
        group.blocks[0],
        2,
        &mut aliases,
        &mut emitted,
    )?;
    if emitted.is_empty() {
        writeln!(out, "        return")?;
    }
    Ok(())
}

fn group_nonlocals(
    unit: &FunctionUnit<'_>,
    group: &BlockGroup,
    groups_by_id: &[&BlockGroup],
    group_by_block: &HashMap<Block, usize>,
    inlineable: &HashSet<usize>,
    state_values: &[Value],
) -> Vec<Value> {
    let state_values: HashSet<Value> = state_values.iter().copied().collect();
    let mut assigned = HashSet::new();
    for group_id in inline_closure(group.id, unit, groups_by_id, group_by_block, inlineable) {
        for &block in &groups_by_id[group_id].blocks {
            for &inst in unit.insts(block) {
                for &result in unit.source.dfg.inst_results(inst) {
                    assigned.insert(result);
                }
            }
        }
    }

    let mut needed: Vec<Value> =
        assigned.into_iter().filter(|value| state_values.contains(value)).collect();
    needed.sort_by_key(|value| format!("{value}"));
    needed
}

fn cross_group_materialized_values(
    unit: &FunctionUnit<'_>,
    group_by_block: &HashMap<Block, usize>,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
) -> HashSet<Value> {
    let mut shared = HashSet::new();
    for &block in &unit.blocks {
        let group = group_by_block[&block];
        for &inst in unit.insts(block) {
            for &arg in unit.source.dfg.insts[inst].arguments(&unit.source.dfg.insts.value_lists) {
                if crosses_group_boundary(
                    unit,
                    arg,
                    group,
                    group_by_block,
                    copy_aliases,
                    expr_aliases,
                ) {
                    collect_materialized_values(
                        unit.source,
                        arg,
                        copy_aliases,
                        expr_aliases,
                        &mut shared,
                    );
                }
            }
        }
    }
    shared
}

fn localized_return_roots(
    unit: &FunctionUnit<'_>,
    cross_group_values: &HashSet<Value>,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
) -> HashSet<Value> {
    let mut localized = HashSet::new();
    for &value in &unit.return_values {
        collect_localized_roots(
            unit.source,
            value,
            cross_group_values,
            copy_aliases,
            expr_aliases,
            &mut localized,
        );
    }
    localized
}

fn shared_state_values(cross_group_values: &HashSet<Value>) -> Vec<Value> {
    let mut shared: Vec<Value> = cross_group_values.iter().copied().collect();
    shared.sort_by_key(|value| format!("{value}"));
    shared
}

fn group_phi_results(unit: &FunctionUnit<'_>, block: Block) -> Vec<Value> {
    unit.insts(block)
        .iter()
        .copied()
        .take_while(|inst| matches!(unit.source.dfg.insts[*inst], InstructionData::PhiNode(_)))
        .filter_map(|inst| unit.source.dfg.inst_results(inst).first().copied())
        .collect()
}

fn compute_inlineable_groups(
    unit: &FunctionUnit<'_>,
    groups: &[BlockGroup],
    group_by_block: &HashMap<Block, usize>,
) -> HashSet<usize> {
    let mut callsite_counts = vec![0usize; groups.len()];
    let mut caller_groups: Vec<HashSet<usize>> = vec![HashSet::new(); groups.len()];
    let mut succs: Vec<HashSet<usize>> = vec![HashSet::new(); groups.len()];

    for group in groups {
        for &block in &group.blocks {
            match unit
                .source
                .layout
                .block_terminator(block)
                .map(|inst| unit.source.dfg.insts[inst].clone())
            {
                Some(InstructionData::Jump { destination }) if unit.contains_block(destination) => {
                    let dst = group_by_block[&destination];
                    if dst != group.id {
                        callsite_counts[dst] += 1;
                        caller_groups[dst].insert(group.id);
                        succs[group.id].insert(dst);
                    }
                }
                Some(InstructionData::Branch { then_dst, else_dst, .. })
                    if unit.contains_block(then_dst) && unit.contains_block(else_dst) =>
                {
                    for destination in [then_dst, else_dst] {
                        let dst = group_by_block[&destination];
                        if dst != group.id {
                            callsite_counts[dst] += 1;
                            caller_groups[dst].insert(group.id);
                            succs[group.id].insert(dst);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let entry_group = group_by_block[&unit.entry];
    let mut inlineable = HashSet::new();
    for group in groups {
        if group.id == entry_group {
            continue;
        }
        if group_has_internal_cycle(unit, group, group_by_block) {
            continue;
        }
        if callsite_counts[group.id] != 1 || caller_groups[group.id].len() != 1 {
            continue;
        }
        let caller = *caller_groups[group.id].iter().next().unwrap();
        if group_reaches(group.id, caller, &succs)
            || group_reaches_successor(group.id, group.id, &succs)
        {
            continue;
        }
        inlineable.insert(group.id);
    }
    inlineable
}

fn group_has_internal_cycle(
    unit: &FunctionUnit<'_>,
    group: &BlockGroup,
    group_by_block: &HashMap<Block, usize>,
) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        Visiting,
        Done,
    }

    fn visit(
        unit: &FunctionUnit<'_>,
        group_id: usize,
        block: Block,
        group_by_block: &HashMap<Block, usize>,
        marks: &mut HashMap<Block, Mark>,
    ) -> bool {
        match marks.get(&block) {
            Some(Mark::Visiting) => return true,
            Some(Mark::Done) => return false,
            None => {}
        }
        marks.insert(block, Mark::Visiting);

        if let Some(inst) = unit.source.layout.block_terminator(block) {
            match unit.source.dfg.insts[inst] {
                InstructionData::Jump { destination } => {
                    if unit.contains_block(destination) && group_by_block[&destination] == group_id
                    {
                        if visit(unit, group_id, destination, group_by_block, marks) {
                            return true;
                        }
                    }
                }
                InstructionData::Branch { then_dst, else_dst, .. } => {
                    for destination in [then_dst, else_dst] {
                        if unit.contains_block(destination)
                            && group_by_block[&destination] == group_id
                        {
                            if visit(unit, group_id, destination, group_by_block, marks) {
                                return true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        marks.insert(block, Mark::Done);
        false
    }

    let mut marks = HashMap::new();
    group
        .blocks
        .iter()
        .copied()
        .any(|block| visit(unit, group.id, block, group_by_block, &mut marks))
}

fn group_reaches(start: usize, target: usize, succs: &[HashSet<usize>]) -> bool {
    let mut stack = vec![start];
    let mut seen = HashSet::new();
    while let Some(group) = stack.pop() {
        if !seen.insert(group) {
            continue;
        }
        if group == target {
            return true;
        }
        stack.extend(succs[group].iter().copied());
    }
    false
}

fn group_reaches_successor(start: usize, target: usize, succs: &[HashSet<usize>]) -> bool {
    let mut stack: Vec<usize> = succs[start].iter().copied().collect();
    let mut seen = HashSet::new();
    while let Some(group) = stack.pop() {
        if !seen.insert(group) {
            continue;
        }
        if group == target {
            return true;
        }
        stack.extend(succs[group].iter().copied());
    }
    false
}

fn inline_closure(
    root: usize,
    unit: &FunctionUnit<'_>,
    groups_by_id: &[&BlockGroup],
    group_by_block: &HashMap<Block, usize>,
    inlineable: &HashSet<usize>,
) -> Vec<usize> {
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    let mut ordered = Vec::new();

    while let Some(group_id) = stack.pop() {
        if !seen.insert(group_id) {
            continue;
        }
        ordered.push(group_id);
        for &block in &groups_by_id[group_id].blocks {
            match unit
                .source
                .layout
                .block_terminator(block)
                .map(|inst| unit.source.dfg.insts[inst].clone())
            {
                Some(InstructionData::Jump { destination }) if unit.contains_block(destination) => {
                    let dst = group_by_block[&destination];
                    if inlineable.contains(&dst) {
                        stack.push(dst);
                    }
                }
                Some(InstructionData::Branch { then_dst, else_dst, .. })
                    if unit.contains_block(then_dst) && unit.contains_block(else_dst) =>
                {
                    for destination in [then_dst, else_dst] {
                        let dst = group_by_block[&destination];
                        if inlineable.contains(&dst) {
                            stack.push(dst);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    ordered
}

fn emit_group_block(
    out: &mut String,
    unit: &FunctionUnit<'_>,
    resolver: &dyn Resolver,
    group: &BlockGroup,
    groups_by_id: &[&BlockGroup],
    group_by_block: &HashMap<Block, usize>,
    inlineable: &HashSet<usize>,
    state_set: &HashSet<Value>,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    block: Block,
    indent: usize,
    aliases: &mut HashMap<Value, String>,
    emitted: &mut HashSet<Block>,
) -> Result<()> {
    if !emitted.insert(block) {
        return transition_to_group(
            out,
            unit,
            resolver,
            groups_by_id,
            group_by_block,
            inlineable,
            state_set,
            copy_aliases,
            expr_aliases,
            localized_roots,
            block,
            block,
            indent,
            aliases,
            emitted,
        );
    }

    for &inst in unit.insts(block) {
        if matches!(unit.source.dfg.insts[inst], InstructionData::PhiNode(_)) {
            continue;
        }
        emit_inst(
            out,
            unit.source,
            resolver,
            inst,
            indent,
            state_set,
            copy_aliases,
            expr_aliases,
            localized_roots,
            aliases,
        )?;
    }

    match unit.source.layout.block_terminator(block).map(|inst| unit.source.dfg.insts[inst].clone())
    {
        Some(InstructionData::Jump { destination }) if unit.contains_block(destination) => {
            if group_by_block[&destination] == group.id {
                emit_group_block(
                    out,
                    unit,
                    resolver,
                    group,
                    groups_by_id,
                    group_by_block,
                    inlineable,
                    state_set,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    destination,
                    indent,
                    aliases,
                    emitted,
                )?;
            } else {
                transition_to_group(
                    out,
                    unit,
                    resolver,
                    groups_by_id,
                    group_by_block,
                    inlineable,
                    state_set,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    destination,
                    block,
                    indent,
                    aliases,
                    emitted,
                )?;
            }
        }
        Some(InstructionData::Branch { cond, then_dst, else_dst, .. })
            if unit.contains_block(then_dst) && unit.contains_block(else_dst) =>
        {
            writeln!(
                out,
                "{}if {}:",
                pad(indent),
                condition_expr(
                    unit.source,
                    resolver,
                    cond,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    aliases,
                )
            )?;
            if group_by_block[&then_dst] == group.id {
                let mut then_aliases = aliases.clone();
                let mut then_emitted = emitted.clone();
                emit_group_block(
                    out,
                    unit,
                    resolver,
                    group,
                    groups_by_id,
                    group_by_block,
                    inlineable,
                    state_set,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    then_dst,
                    indent + 1,
                    &mut then_aliases,
                    &mut then_emitted,
                )?;
            } else {
                let mut then_aliases = aliases.clone();
                let mut then_emitted = emitted.clone();
                transition_to_group(
                    out,
                    unit,
                    resolver,
                    groups_by_id,
                    group_by_block,
                    inlineable,
                    state_set,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    then_dst,
                    block,
                    indent + 1,
                    &mut then_aliases,
                    &mut then_emitted,
                )?;
            }
            writeln!(out, "{}else:", pad(indent))?;
            if group_by_block[&else_dst] == group.id {
                let mut else_aliases = aliases.clone();
                let mut else_emitted = emitted.clone();
                emit_group_block(
                    out,
                    unit,
                    resolver,
                    group,
                    groups_by_id,
                    group_by_block,
                    inlineable,
                    state_set,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    else_dst,
                    indent + 1,
                    &mut else_aliases,
                    &mut else_emitted,
                )?;
            } else {
                let mut else_aliases = aliases.clone();
                let mut else_emitted = emitted.clone();
                transition_to_group(
                    out,
                    unit,
                    resolver,
                    groups_by_id,
                    group_by_block,
                    inlineable,
                    state_set,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    else_dst,
                    block,
                    indent + 1,
                    &mut else_aliases,
                    &mut else_emitted,
                )?;
            }
        }
        Some(InstructionData::Exit) | None => {
            writeln!(out, "{}return", pad(indent))?;
        }
        Some(other) => {
            writeln!(out, "{}mir_unlifted({:?})", pad(indent), format!("{other:?}"))?;
            writeln!(out, "{}return", pad(indent))?;
        }
    }

    Ok(())
}

fn transition_to_group(
    out: &mut String,
    unit: &FunctionUnit<'_>,
    resolver: &dyn Resolver,
    groups_by_id: &[&BlockGroup],
    group_by_block: &HashMap<Block, usize>,
    inlineable: &HashSet<usize>,
    state_set: &HashSet<Value>,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    destination: Block,
    pred: Block,
    indent: usize,
    aliases: &mut HashMap<Value, String>,
    emitted: &mut HashSet<Block>,
) -> Result<()> {
    let args = group_phi_results(unit, destination)
        .into_iter()
        .filter_map(|result| {
            let inst = match unit.source.dfg.value_def(result) {
                ValueDef::Result(inst, 0) => inst,
                _ => return None,
            };
            let InstructionData::PhiNode(phi) = &unit.source.dfg.insts[inst] else {
                return None;
            };
            unit.source.dfg.phi_edge_val(phi, pred).map(|value| {
                value_expr(
                    unit.source,
                    value,
                    resolver,
                    copy_aliases,
                    expr_aliases,
                    localized_roots,
                    aliases,
                )
            })
        })
        .collect::<Vec<_>>();
    let destination_group = group_by_block[&destination];
    if inlineable.contains(&destination_group) {
        debug_assert!(
            !emitted.contains(&destination),
            "inlineable group {} unexpectedly requires fallback emission",
            destination_group
        );
        for (result, arg) in group_phi_results(unit, destination).into_iter().zip(args.into_iter())
        {
            if state_set.contains(&result) {
                writeln!(out, "{}{} = {}", pad(indent), value_name(result), arg)?;
                maybe_store_localized_root(
                    out,
                    result,
                    arg.as_str(),
                    copy_aliases,
                    localized_roots,
                    indent,
                )?;
            } else {
                aliases.insert(result, arg);
                maybe_store_localized_alias(
                    out,
                    result,
                    copy_aliases,
                    expr_aliases,
                    aliases,
                    localized_roots,
                    indent,
                )?;
            }
        }
        return emit_group_block(
            out,
            unit,
            resolver,
            groups_by_id[destination_group],
            groups_by_id,
            group_by_block,
            inlineable,
            state_set,
            copy_aliases,
            expr_aliases,
            localized_roots,
            destination,
            indent,
            aliases,
            emitted,
        );
    }
    let args = args.join(", ");
    writeln!(out, "{}return _fn_{}({})", pad(indent), destination_group, args)?;
    Ok(())
}

fn emit_inst(
    out: &mut String,
    function: &Function,
    resolver: &dyn Resolver,
    inst: Inst,
    indent: usize,
    state_set: &HashSet<Value>,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    aliases: &mut HashMap<Value, String>,
) -> Result<()> {
    let data = function.dfg.insts[inst].clone();
    if matches!(data, InstructionData::PhiNode(_)) {
        return Ok(());
    }

    if data.is_terminator() {
        return Ok(());
    }

    let rendered = function.dfg.display_inst(inst).to_string();
    let results = function.dfg.inst_results(inst);
    match lift_inst(
        function,
        resolver,
        inst,
        &data,
        copy_aliases,
        expr_aliases,
        localized_roots,
        aliases,
    ) {
        Some(expr) if results.len() == 1 => {
            if !state_set.contains(&results[0])
                && (copy_aliases.contains_key(&results[0])
                    || expr_aliases.contains_key(&results[0])
                    || can_inline_expr_alias(&data, &expr))
            {
                aliases.insert(results[0], expr);
                maybe_store_localized_alias(
                    out,
                    results[0],
                    copy_aliases,
                    expr_aliases,
                    aliases,
                    localized_roots,
                    indent,
                )?;
            } else {
                writeln!(out, "{}{} = {}", pad(indent), value_name(results[0]), expr)?;
                maybe_store_localized_root(
                    out,
                    results[0],
                    &value_name(results[0]),
                    copy_aliases,
                    localized_roots,
                    indent,
                )?;
            }
        }
        Some(expr) if results.is_empty() => {
            writeln!(out, "{}{}", pad(indent), expr)?;
        }
        Some(expr) => {
            let lhs = results.iter().map(|value| value_name(*value)).collect::<Vec<_>>().join(", ");
            writeln!(out, "{}{} = {}", pad(indent), lhs, expr)?;
        }
        None if results.is_empty() => {
            writeln!(out, "{}# MIR: {}", pad(indent), rendered)?;
            writeln!(out, "{}mir_unlifted({rendered:?})", pad(indent))?;
        }
        None if results.len() == 1 => {
            writeln!(out, "{}# MIR: {}", pad(indent), rendered)?;
            writeln!(
                out,
                "{}{} = mir_unlifted({rendered:?})",
                pad(indent),
                value_name(results[0])
            )?;
        }
        None => {
            writeln!(out, "{}# MIR: {}", pad(indent), rendered)?;
            let lhs = results.iter().map(|value| value_name(*value)).collect::<Vec<_>>().join(", ");
            writeln!(out, "{}{} = mir_unlifted({rendered:?})", pad(indent), lhs)?;
        }
    }

    Ok(())
}

fn lift_inst(
    function: &Function,
    resolver: &dyn Resolver,
    inst: Inst,
    data: &InstructionData,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    aliases: &HashMap<Value, String>,
) -> Option<String> {
    match data {
        InstructionData::Unary { opcode, arg } => {
            let arg = value_expr(
                function,
                *arg,
                resolver,
                copy_aliases,
                expr_aliases,
                localized_roots,
                aliases,
            );
            unary_expr(*opcode, arg)
        }
        InstructionData::Binary { opcode, args } => {
            let lhs = value_expr(
                function,
                args[0],
                resolver,
                copy_aliases,
                expr_aliases,
                localized_roots,
                aliases,
            );
            let rhs = value_expr(
                function,
                args[1],
                resolver,
                copy_aliases,
                expr_aliases,
                localized_roots,
                aliases,
            );
            binary_expr(*opcode, lhs, rhs)
        }
        InstructionData::Call { func_ref, args } => {
            let target = call_name(function, *func_ref);
            let args = args
                .as_slice(&function.dfg.insts.value_lists)
                .iter()
                .map(|arg| {
                    value_expr(
                        function,
                        *arg,
                        resolver,
                        copy_aliases,
                        expr_aliases,
                        localized_roots,
                        aliases,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("{target}({args})"))
        }
        InstructionData::Jump { .. }
        | InstructionData::Branch { .. }
        | InstructionData::PhiNode(_) => None,
        InstructionData::Exit => {
            let _ = inst;
            None
        }
    }
}

fn condition_expr(
    function: &Function,
    resolver: &dyn Resolver,
    cond: Value,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    aliases: &HashMap<Value, String>,
) -> String {
    value_expr(function, cond, resolver, copy_aliases, expr_aliases, localized_roots, aliases)
}

fn unary_expr(opcode: Opcode, arg: String) -> Option<String> {
    let expr = match opcode {
        Opcode::Inot | Opcode::Bnot => format!("not ({arg})"),
        Opcode::Fneg | Opcode::Ineg => format!("-({arg})"),
        Opcode::FIcast | Opcode::BIcast => format!("int({arg})"),
        Opcode::IFcast | Opcode::BFcast => format!("float({arg})"),
        Opcode::IBcast | Opcode::FBcast => format!("bool({arg})"),
        Opcode::OptBarrier => arg,
        Opcode::Sqrt => format!("math.sqrt({arg})"),
        Opcode::Exp => format!("math.exp({arg})"),
        Opcode::Ln => format!("math.log({arg})"),
        Opcode::Log => format!("math.log10({arg})"),
        Opcode::Clog2 => format!("math.ceil(math.log2({arg}))"),
        Opcode::Floor => format!("math.floor({arg})"),
        Opcode::Ceil => format!("math.ceil({arg})"),
        Opcode::Sin => format!("math.sin({arg})"),
        Opcode::Cos => format!("math.cos({arg})"),
        Opcode::Tan => format!("math.tan({arg})"),
        Opcode::Asin => format!("math.asin({arg})"),
        Opcode::Acos => format!("math.acos({arg})"),
        Opcode::Atan => format!("math.atan({arg})"),
        Opcode::Sinh => format!("math.sinh({arg})"),
        Opcode::Cosh => format!("math.cosh({arg})"),
        Opcode::Tanh => format!("math.tanh({arg})"),
        Opcode::Asinh => format!("math.asinh({arg})"),
        Opcode::Acosh => format!("math.acosh({arg})"),
        Opcode::Atanh => format!("math.atanh({arg})"),
        _ => return None,
    };
    Some(expr)
}

fn binary_expr(opcode: Opcode, lhs: String, rhs: String) -> Option<String> {
    let expr = match opcode {
        Opcode::Iadd | Opcode::Fadd => format!("({lhs}) + ({rhs})"),
        Opcode::Isub | Opcode::Fsub => format!("({lhs}) - ({rhs})"),
        Opcode::Imul | Opcode::Fmul => format!("({lhs}) * ({rhs})"),
        Opcode::Idiv | Opcode::Fdiv => format!("({lhs}) / ({rhs})"),
        Opcode::Irem | Opcode::Frem => format!("({lhs}) % ({rhs})"),
        Opcode::Ishl => format!("({lhs}) << ({rhs})"),
        Opcode::Ishr => format!("({lhs}) >> ({rhs})"),
        Opcode::Ixor => format!("({lhs}) ^ ({rhs})"),
        Opcode::Iand => format!("({lhs}) & ({rhs})"),
        Opcode::Ior => format!("({lhs}) | ({rhs})"),
        Opcode::Ilt | Opcode::Flt => format!("({lhs}) < ({rhs})"),
        Opcode::Igt | Opcode::Fgt => format!("({lhs}) > ({rhs})"),
        Opcode::Ige | Opcode::Fge => format!("({lhs}) >= ({rhs})"),
        Opcode::Ile | Opcode::Fle => format!("({lhs}) <= ({rhs})"),
        Opcode::Ieq | Opcode::Feq | Opcode::Seq | Opcode::Beq => format!("({lhs}) == ({rhs})"),
        Opcode::Ine | Opcode::Fne | Opcode::Sne | Opcode::Bne => format!("({lhs}) != ({rhs})"),
        Opcode::Hypot => format!("math.hypot({lhs}, {rhs})"),
        Opcode::Atan2 => format!("math.atan2({lhs}, {rhs})"),
        Opcode::Pow => format!("math.pow({lhs}, {rhs})"),
        _ => return None,
    };
    Some(expr)
}

fn emit_return_values(
    out: &mut String,
    function: &Function,
    values: &[Value],
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    resolver: &dyn Resolver,
    indent: usize,
) -> Result<()> {
    let entries = values
        .iter()
        .copied()
        .into_iter()
        .map(|value| {
            let expr = return_value_expr(
                function,
                value,
                resolver,
                copy_aliases,
                expr_aliases,
                localized_roots,
                &HashMap::new(),
            );
            format!("{:?}: {expr}", value_name(value))
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "{}return {{{entries}}}", pad(indent))?;
    Ok(())
}

fn return_value_expr(
    function: &Function,
    value: Value,
    resolver: &dyn Resolver,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    aliases: &HashMap<Value, String>,
) -> String {
    let value = canonical_value(value, copy_aliases);
    if localized_roots.contains(&value) {
        return format!("_ret[{:#?}]", value_name(value));
    }
    if let Some(alias) = aliases.get(&value) {
        return alias.clone();
    }
    if let Some(alias) = expr_aliases.get(&value) {
        let arg = return_value_expr(
            function,
            alias.arg,
            resolver,
            copy_aliases,
            expr_aliases,
            localized_roots,
            aliases,
        );
        return unary_expr(alias.opcode, arg).unwrap_or_else(|| value_name(value));
    }
    value_expr_no_alias(function, value, resolver)
}

fn call_name(function: &Function, func_ref: FuncRef) -> String {
    let sig = &function.dfg.signatures[func_ref];
    if sig.name.is_empty() {
        sanitize_ident(&format!("func_ref_{}", usize::from(func_ref)))
    } else {
        sanitize_ident(&sig.name)
    }
}

fn value_expr(
    function: &Function,
    value: Value,
    resolver: &dyn Resolver,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    localized_roots: &HashSet<Value>,
    aliases: &HashMap<Value, String>,
) -> String {
    if let Some(alias) = aliases.get(&value) {
        return alias.clone();
    }
    let value = canonical_value(value, copy_aliases);
    if let Some(alias) = expr_aliases.get(&value) {
        let arg = value_expr(
            function,
            alias.arg,
            resolver,
            copy_aliases,
            expr_aliases,
            localized_roots,
            aliases,
        );
        return unary_expr(alias.opcode, arg).unwrap_or_else(|| value_name(value));
    }
    let _ = localized_roots;
    value_expr_no_alias(function, value, resolver)
}

fn value_expr_no_alias(function: &Function, value: Value, resolver: &dyn Resolver) -> String {
    match function.dfg.value_def(value) {
        ValueDef::Const(Const::Int(val)) => val.to_string(),
        ValueDef::Const(Const::Float(val)) => format!("{}", f64::from(val)),
        ValueDef::Const(Const::Str(val)) => format!("{:?}", resolver.resolve(&val)),
        ValueDef::Const(Const::Bool(val)) => {
            if val {
                "True".to_owned()
            } else {
                "False".to_owned()
            }
        }
        ValueDef::Param(_) | ValueDef::Result(_, _) => value_name(value),
        ValueDef::Invalid => "None".to_owned(),
    }
}

fn compute_copy_aliases(unit: &FunctionUnit<'_>) -> HashMap<Value, Value> {
    let mut aliases = HashMap::new();
    for &block in &unit.blocks {
        for &inst in unit.insts(block) {
            let Some(&result) = unit.source.dfg.inst_results(inst).first() else {
                continue;
            };
            if let Some(source) = direct_copy_source(unit.source, inst) {
                aliases.insert(result, source);
            }
        }
    }

    let keys: Vec<Value> = aliases.keys().copied().collect();
    for value in keys {
        let canonical = canonical_value(value, &aliases);
        aliases.insert(value, canonical);
    }
    aliases
}

fn compute_expr_aliases(unit: &FunctionUnit<'_>) -> HashMap<Value, ExprAlias> {
    let mut aliases = HashMap::new();
    for &block in &unit.blocks {
        for &inst in unit.insts(block) {
            let Some(&result) = unit.source.dfg.inst_results(inst).first() else {
                continue;
            };
            let InstructionData::Unary { opcode, arg } = unit.source.dfg.insts[inst] else {
                continue;
            };
            if !is_inline_alias_opcode(opcode) || opcode == Opcode::OptBarrier {
                continue;
            }
            aliases.insert(result, ExprAlias { opcode, arg });
        }
    }
    aliases
}

fn direct_copy_source(function: &Function, inst: Inst) -> Option<Value> {
    match function.dfg.insts[inst] {
        InstructionData::Unary { opcode: Opcode::OptBarrier, arg } => Some(arg),
        _ => None,
    }
}

fn canonical_value(mut value: Value, aliases: &HashMap<Value, Value>) -> Value {
    let mut seen = HashSet::new();
    while let Some(&next) = aliases.get(&value) {
        if !seen.insert(value) || next == value {
            break;
        }
        value = next;
    }
    value
}

fn is_inline_alias_opcode(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Inot
            | Opcode::Bnot
            | Opcode::Fneg
            | Opcode::Ineg
            | Opcode::FIcast
            | Opcode::BIcast
            | Opcode::IFcast
            | Opcode::BFcast
            | Opcode::IBcast
            | Opcode::FBcast
            | Opcode::OptBarrier
    )
}

fn crosses_group_boundary(
    unit: &FunctionUnit<'_>,
    value: Value,
    group: usize,
    group_by_block: &HashMap<Block, usize>,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
) -> bool {
    let value = canonical_value(value, copy_aliases);
    if let Some(alias) = expr_aliases.get(&value) {
        return crosses_group_boundary(
            unit,
            alias.arg,
            group,
            group_by_block,
            copy_aliases,
            expr_aliases,
        );
    }
    let ValueDef::Result(def_inst, _) = unit.source.dfg.value_def(value) else {
        return false;
    };
    let Some(def_block) = unit.source.layout.inst_block(def_inst) else {
        return false;
    };
    unit.contains_block(def_block) && group_by_block[&def_block] != group
}

fn collect_materialized_values(
    function: &Function,
    value: Value,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    out: &mut HashSet<Value>,
) {
    let value = canonical_value(value, copy_aliases);
    if let Some(alias) = expr_aliases.get(&value) {
        collect_materialized_values(function, alias.arg, copy_aliases, expr_aliases, out);
        return;
    }
    match function.dfg.value_def(value) {
        ValueDef::Const(_) | ValueDef::Invalid => {}
        ValueDef::Param(_) | ValueDef::Result(_, _) => {
            out.insert(value);
        }
    }
}

fn collect_localized_roots(
    function: &Function,
    value: Value,
    cross_group_values: &HashSet<Value>,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    out: &mut HashSet<Value>,
) {
    let value = canonical_value(value, copy_aliases);
    if let Some(alias) = expr_aliases.get(&value) {
        collect_localized_roots(
            function,
            alias.arg,
            cross_group_values,
            copy_aliases,
            expr_aliases,
            out,
        );
        return;
    }
    match function.dfg.value_def(value) {
        ValueDef::Param(_) | ValueDef::Const(_) | ValueDef::Invalid => {}
        ValueDef::Result(_, _) if !cross_group_values.contains(&value) => {
            out.insert(value);
        }
        ValueDef::Result(_, _) => {}
    }
}

fn maybe_store_localized_root(
    out: &mut String,
    value: Value,
    expr: &str,
    copy_aliases: &HashMap<Value, Value>,
    localized_roots: &HashSet<Value>,
    indent: usize,
) -> Result<()> {
    let root = canonical_value(value, copy_aliases);
    if localized_roots.contains(&root) && root == value {
        writeln!(out, "{}_ret[{:#?}] = {}", pad(indent), value_name(root), expr)?;
    }
    Ok(())
}

fn maybe_store_localized_alias(
    out: &mut String,
    value: Value,
    copy_aliases: &HashMap<Value, Value>,
    expr_aliases: &HashMap<Value, ExprAlias>,
    aliases: &HashMap<Value, String>,
    localized_roots: &HashSet<Value>,
    indent: usize,
) -> Result<()> {
    let root = canonical_value(value, copy_aliases);
    if localized_roots.contains(&root) && root == value {
        if let Some(expr) = aliases.get(&value) {
            writeln!(out, "{}_ret[{:#?}] = {}", pad(indent), value_name(root), expr)?;
        }
    } else if localized_roots.contains(&root) && expr_aliases.contains_key(&value) {
        let _ = aliases;
    }
    Ok(())
}

fn value_name(value: Value) -> String {
    format!("{value}").replace('.', "_")
}

fn phi_param_name(value: Value) -> String {
    format!("phi_{}", value_name(value))
}

fn is_simple_name(expr: &str) -> bool {
    let mut chars = expr.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn can_inline_expr_alias(data: &InstructionData, expr: &str) -> bool {
    if is_simple_name(expr) {
        return true;
    }

    matches!(
        data,
        InstructionData::Unary {
            opcode: Opcode::Inot
                | Opcode::Bnot
                | Opcode::Fneg
                | Opcode::Ineg
                | Opcode::FIcast
                | Opcode::BIcast
                | Opcode::IFcast
                | Opcode::BFcast
                | Opcode::IBcast
                | Opcode::FBcast
                | Opcode::OptBarrier,
            ..
        }
    )
}

fn sanitize_ident(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }

    if out.is_empty() {
        out.push_str("lifted");
    }
    if out.as_bytes()[0].is_ascii_digit() {
        out.insert(0, '_');
    }
    out
}

fn unique_name(base: &str, index: usize, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_owned()) {
        return base.to_owned();
    }

    let mut candidate = format!("{base}_{index}");
    let mut suffix = index + 1;
    while !used.insert(candidate.clone()) {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    candidate
}

fn param_index(param: Param) -> usize {
    usize::from(param)
}

fn pad(indent: usize) -> String {
    "    ".repeat(indent)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use hir_lower::HirInterner;
    use mir::{Function, Value};

    use super::{
        dump_function_lir_with_returns_and_captures,
        lift_function_with_hir_returns_and_param_hints, lift_text, normalize_mir_input,
    };

    #[test]
    fn lifts_sample_mir_to_python() {
        let input = include_str!("../../test_data/mir/case.mir");
        let lifted = lift_text(input, None).unwrap();
        assert!(lifted.contains("def lifted("));
        assert!(!lifted.contains("_pc"));
        assert!(!lifted.contains("_lir_target"));
        assert!(!lifted.contains("_lir_result[0]"));
        assert!(!lifted.contains("(\"call\","));
        assert!(!lifted.contains("(\"return\","));
        assert!(lifted.contains("return _lifted_entry("));
        assert!(!lifted.contains(" = None\n"));
        assert!(!lifted.contains("nonlocal "));
        assert!(lifted.contains("def _lifted_bb_"));
        assert!(lifted.contains("math.sin"));
        assert!(!lifted.contains("phi_v19_from_block2"));
        assert!(!lifted.contains("v36 = v35"));
        assert!(lifted.contains("if (v16) < (0):"));
        assert!(lifted.contains("v31 = (float(v16)) / (3.141)"));
        assert!(lifted.contains("_lir_return[") && lifted.contains("] = v36"));
    }

    #[test]
    fn slices_functions_from_metadata() {
        let input = include_str!("../../test_data/mir/case.mir");
        let metadata = r#"{
            "functions": [
                {"name": "piece", "blocks": ["block2", "block4"]}
            ]
        }"#;
        let lifted = lift_text(input, Some(metadata)).unwrap();
        assert!(lifted.contains("def piece("));
        assert!(lifted.contains("def piece(v16, v20):"));
    }

    #[test]
    fn dumps_lir_text() {
        let input = include_str!("../../test_data/mir/case.mir");
        let dumped = super::dump_lir_text(input, None).unwrap();
        assert!(dumped.contains("fn lifted("));
        assert!(dumped.contains("bb0:"));
        assert!(dumped.contains("return {"));
    }

    #[test]
    fn applies_param_name_hints_only_when_provided() {
        let input = r#"
function %param_hint(v0, v1) {
                                block0:
}
"#;
        let (functions, interner) =
            mir_reader::parse_functions(&normalize_mir_input(input)).unwrap();
        let function = &functions[0];

        let standalone = lift_text(input, None).unwrap();
        assert!(standalone.contains("def param_hint(v0, v1):"));

        let mut hints = HashMap::new();
        hints.insert(mir::Param::from(0usize), "param_width".to_owned());
        let lifted = lift_function_with_hir_returns_and_param_hints(
            function,
            &[],
            &interner,
            &HirInterner::default(),
            hints,
        )
        .unwrap();

        assert!(lifted.contains("def param_hint(param_width, v1):"));
    }

    #[test]
    fn captures_phi_values_on_incoming_edges() {
        let input = r#"
function %phi_capture(v0, v1, v2) {
                                block0:
                                    br v0, block1, block2

                                block1:
                                    jmp block3

                                block2:
                                    jmp block3

                                block3:
                                    v3 = phi [v1, block1], [v2, block2]
                                    jmp block4

                                block4:
}
"#;
        let (functions, interner) =
            mir_reader::parse_functions(&normalize_mir_input(input)).unwrap();
        let function = &functions[0];
        let capture = value_by_name(function, "v3");
        let lir = dump_function_lir_with_returns_and_captures(function, &[], &[capture], &interner)
            .unwrap();

        assert_eq!(lir.matches("capture \"v3\"").count(), 2);
    }

    #[test]
    fn captures_entry_defined_params_and_constants() {
        let input = r#"
function %entry_capture(v0) {
    v1 = iconst 2
                                block0:
}
"#;
        let (functions, interner) =
            mir_reader::parse_functions(&normalize_mir_input(input)).unwrap();
        let function = &functions[0];
        let param = value_by_name(function, "v0");
        let constant = value_by_name(function, "v1");
        let lir = dump_function_lir_with_returns_and_captures(
            function,
            &[],
            &[param, constant],
            &interner,
        )
        .unwrap();

        assert!(lir.contains("capture \"v0\""));
        assert!(lir.contains("capture \"v1\" = 2"));
    }

    fn value_by_name(function: &Function, name: &str) -> Value {
        function
            .dfg
            .values()
            .find(|value| value.to_string() == name)
            .unwrap_or_else(|| panic!("missing MIR value {name}"))
    }
}
