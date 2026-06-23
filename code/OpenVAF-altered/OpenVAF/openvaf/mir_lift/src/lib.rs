use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use hir_lower::HirInterner;
use lasso::Resolver;
use mir::{Block, Function, Inst, Param, Value, ValueDef};
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

pub fn lift_function_with_hir_returns_param_and_output_hints(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
) -> Result<String> {
    lift_function_with_hir_returns_param_output_and_type_hints(
        function,
        return_values,
        resolver,
        intern,
        param_name_hints,
        output_name_hints,
        HashMap::new(),
    )
}

pub fn lift_function_with_hir_returns_param_output_and_type_hints(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
    output_type_hints: HashMap<Value, lir::LirType>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_param_and_output_hints(
        function,
        return_values,
        intern,
        param_name_hints,
        output_name_hints,
        output_type_hints,
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

pub fn lift_function_with_hir_returns_captures_param_and_output_hints(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
) -> Result<String> {
    lift_function_with_hir_returns_captures_param_output_and_type_hints(
        function,
        return_values,
        capture_values,
        resolver,
        intern,
        param_name_hints,
        output_name_hints,
        HashMap::new(),
    )
}

pub fn lift_function_with_hir_returns_captures_param_output_and_type_hints(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
    output_type_hints: HashMap<Value, lir::LirType>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_captures_param_and_output_hints(
        function,
        return_values,
        capture_values,
        intern,
        param_name_hints,
        output_name_hints,
        output_type_hints,
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

pub fn dump_function_lir_with_hir_returns_param_and_output_hints(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
) -> Result<String> {
    dump_function_lir_with_hir_returns_param_output_and_type_hints(
        function,
        return_values,
        resolver,
        intern,
        param_name_hints,
        output_name_hints,
        HashMap::new(),
    )
}

pub fn dump_function_lir_with_hir_returns_param_output_and_type_hints(
    function: &Function,
    return_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
    output_type_hints: HashMap<Value, lir::LirType>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_param_and_output_hints(
        function,
        return_values,
        intern,
        param_name_hints,
        output_name_hints,
        output_type_hints,
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

pub fn dump_function_lir_with_hir_returns_captures_param_and_output_hints(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
) -> Result<String> {
    dump_function_lir_with_hir_returns_captures_param_output_and_type_hints(
        function,
        return_values,
        capture_values,
        resolver,
        intern,
        param_name_hints,
        output_name_hints,
        HashMap::new(),
    )
}

pub fn dump_function_lir_with_hir_returns_captures_param_output_and_type_hints(
    function: &Function,
    return_values: &[Value],
    capture_values: &[Value],
    resolver: &dyn Resolver,
    intern: &HirInterner,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
    output_type_hints: HashMap<Value, lir::LirType>,
) -> Result<String> {
    let unit = FunctionUnit::whole_with_hir_returns_captures_param_and_output_hints(
        function,
        return_values,
        capture_values,
        intern,
        param_name_hints,
        output_name_hints,
        output_type_hints,
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
    insts_by_block: HashMap<Block, Vec<Inst>>,
    entry: Block,
    params: Vec<Value>,
    return_values: Vec<Value>,
    capture_values: Vec<Value>,
    callbacks: Option<&'a HirInterner>,
    param_name_hints: HashMap<Param, String>,
    output_name_hints: HashMap<Value, String>,
    output_type_hints: HashMap<Value, lir::LirType>,
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
        let params = collect_function_params(source);
        let return_values = collect_return_values(source, &blocks, &insts_by_block, &params);
        Ok(Self {
            name,
            source,
            blocks,
            block_set,
            insts_by_block,
            entry,
            params,
            return_values,
            capture_values: Vec::new(),
            callbacks: None,
            param_name_hints: HashMap::new(),
            output_name_hints: HashMap::new(),
            output_type_hints: HashMap::new(),
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
        let params = collect_function_params(source);
        let return_values = live_return_values(source, &blocks, return_values);
        let capture_values = live_return_values(source, &blocks, capture_values);
        Ok(Self {
            name,
            source,
            blocks,
            block_set,
            insts_by_block,
            entry,
            params,
            return_values,
            capture_values,
            callbacks: None,
            param_name_hints: HashMap::new(),
            output_name_hints: HashMap::new(),
            output_type_hints: HashMap::new(),
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

    fn whole_with_hir_returns_param_and_output_hints(
        source: &'a Function,
        return_values: &[Value],
        callbacks: &'a HirInterner,
        param_name_hints: HashMap<Param, String>,
        output_name_hints: HashMap<Value, String>,
        output_type_hints: HashMap<Value, lir::LirType>,
    ) -> Result<Self> {
        let mut unit = Self::whole_with_hir_returns_and_param_hints(
            source,
            return_values,
            callbacks,
            param_name_hints,
        )?;
        unit.output_name_hints = output_name_hints;
        unit.output_type_hints = output_type_hints;
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

    fn whole_with_hir_returns_captures_param_and_output_hints(
        source: &'a Function,
        return_values: &[Value],
        capture_values: &[Value],
        callbacks: &'a HirInterner,
        param_name_hints: HashMap<Param, String>,
        output_name_hints: HashMap<Value, String>,
        output_type_hints: HashMap<Value, lir::LirType>,
    ) -> Result<Self> {
        let mut unit = Self::whole_with_hir_returns_captures_and_param_hints(
            source,
            return_values,
            capture_values,
            callbacks,
            param_name_hints,
        )?;
        unit.output_name_hints = output_name_hints;
        unit.output_type_hints = output_type_hints;
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
        let params = collect_slice_params(source, &block_set);
        let return_values = collect_return_values(source, &blocks, &insts_by_block, &params);

        Ok(Self {
            name: sanitize_ident(&spec.name),
            source,
            blocks,
            block_set,
            insts_by_block,
            entry,
            params,
            return_values,
            capture_values: Vec::new(),
            callbacks: None,
            param_name_hints: HashMap::new(),
            output_name_hints: HashMap::new(),
            output_type_hints: HashMap::new(),
        })
    }

    fn contains_block(&self, block: Block) -> bool {
        self.block_set.contains(&block)
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use hir_lower::HirInterner;
    use mir::{Function, Value};

    use super::{
        dump_function_lir_with_returns_and_captures, emit_function_unit,
        lift_function_with_hir_returns_and_param_hints, lift_text, normalize_mir_input,
        FunctionUnit,
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
        assert!(lifted.contains("_lir_result.v36 = v36"));
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
function %param_hint(v0, v1, v2) {
                                block0:
}
"#;
        let (functions, interner) =
            mir_reader::parse_functions(&normalize_mir_input(input)).unwrap();
        let function = &functions[0];

        let standalone = lift_text(input, None).unwrap();
        assert!(standalone.contains("def param_hint(v0, v1, v2):"));

        let mut hints = HashMap::new();
        hints.insert(mir::Param::from(0usize), "param_width".to_owned());
        hints.insert(mir::Param::from(1usize), "given_width".to_owned());
        hints.insert(mir::Param::from(2usize), "builtin_mfactor".to_owned());
        let lifted = lift_function_with_hir_returns_and_param_hints(
            function,
            &[],
            &interner,
            &HirInterner::default(),
            hints,
        )
        .unwrap();

        assert!(lifted.contains("def param_hint(param_width, given_width, builtin_mfactor):"));
    }

    #[test]
    fn names_simparam_opt_results_from_constant_name() {
        let input = r#"
function %simparam_name() {
    fn0 = const fn %simparam_opt(2) -> 1
    v0 = sconst "gmin"
    v1 = fconst 0x1.19799812dea11p-40
                                block0:
                                    v2 = call fn0(v0, v1)
                                    v3 = optbarrier v2
                                    jmp block1

                                block1:
}
"#;

        let lifted = lift_text(input, None).unwrap();
        assert!(lifted.contains("simparam_gmin = _lir_simparam_opt(\"gmin\", 0.000000000001)"));
    }

    #[test]
    fn variable_taint_names_generic_temps_by_lowering_origin() {
        let input = r#"
function %taint_names(v0, v1, v2) {
                                block0:
                                    v3 = fadd v0, v1
                                    v4 = fadd v2, v0
                                    v5 = fadd v3, v4
}
"#;
        let (functions, interner) =
            mir_reader::parse_functions(&normalize_mir_input(input)).unwrap();
        let function = &functions[0];
        let returns = ["v3", "v4", "v5"]
            .into_iter()
            .map(|name| value_by_name(function, name))
            .collect::<Vec<_>>();

        let mut hints = HashMap::new();
        hints.insert(mir::Param::from(0usize), "param_left".to_owned());
        hints.insert(mir::Param::from(1usize), "given_right".to_owned());
        let mut unit = FunctionUnit::whole_with_returns(function, &returns).unwrap();
        unit.param_name_hints = hints;
        let lifted = emit_function_unit(&unit, &interner).unwrap();

        assert!(lifted.contains("v3 = (param_left) + (given_right)"), "{lifted}");
        assert!(lifted.contains("v4 = (v2) + (param_left)"), "{lifted}");
        assert!(
            lifted.contains("v5 = ((param_left) + (given_right)) + ((v2) + (param_left))"),
            "{lifted}"
        );
        assert!(!lifted.contains("p4 ="), "{lifted}");
        assert!(!lifted.contains("p5 ="), "{lifted}");
    }

    #[test]
    fn variable_taint_keeps_generic_call_result_variable_named() {
        let input = r#"
function %call_taint(v0) {
    fn0 = const fn %simparam_opt(2) -> 1
    v1 = fconst 0x1.0000000000000p0
                                block0:
                                    v2 = call fn0(v0, v1)
}
"#;
        let (functions, interner) =
            mir_reader::parse_functions(&normalize_mir_input(input)).unwrap();
        let function = &functions[0];
        let ret = value_by_name(function, "v2");

        let mut hints = HashMap::new();
        hints.insert(mir::Param::from(0usize), "param_name".to_owned());
        let mut unit = FunctionUnit::whole_with_returns(function, &[ret]).unwrap();
        unit.param_name_hints = hints;
        let lifted = emit_function_unit(&unit, &interner).unwrap();

        assert!(lifted.contains("v2 = _lir_simparam_opt(param_name, 1"), "{lifted}");
        assert!(!lifted.contains("p2 = _lir_simparam_opt"), "{lifted}");
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
