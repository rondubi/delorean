use std::fs::{create_dir_all, remove_file};
use std::io::Write;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use basedb::diagnostics::{ConsoleSink, DiagnosticSink};
use basedb::BaseDB;
use camino::Utf8PathBuf;
use hir::CompilationDB;
use lasso::Rodeo;
use linker::link;
use mir_llvm::LLVMBackend;
use sim_back::collect_modules;
use sim_back::{print_intern, print_module};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

pub use basedb::lints::builtin as builtin_lints;
pub use basedb::lints::LintLevel;
pub use llvm::OptLevel;
pub use paths::AbsPathBuf;
pub use target::host_triple;
pub use target::spec::{get_target_names, Target};

mod cache;
pub mod elysian;

use basedb::CliParamDefault;

#[derive(Debug, Clone)]
pub enum CompilationDestination {
    Path { lib_file: Utf8PathBuf },
    Cache { cache_dir: Utf8PathBuf },
}

pub enum CompilationTermination {
    Compiled { lib_file: Utf8PathBuf },
    FatalDiagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodegenBackend {
    Llvm,
    MirLift,
}

impl CodegenBackend {
    pub fn output_extension(self) -> &'static str {
        match self {
            CodegenBackend::Llvm => "osdi",
            CodegenBackend::MirLift => "py",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Opts {
    pub dry_run: bool,
    pub defines: Vec<String>,
    pub codegen_opts: Vec<String>,
    pub lints: Vec<(String, LintLevel)>,
    pub input: Utf8PathBuf,
    pub output: CompilationDestination,
    pub include: Vec<AbsPathBuf>,
    pub opt_lvl: OptLevel,
    pub target: Target,
    pub target_cpu: String,
    pub backend: CodegenBackend,
    pub dump_mir: bool,
    pub dump_unopt_mir: bool,
    pub dump_ir: bool,
    pub dump_unopt_ir: bool,
    pub dump_lir: bool,
    // RDUBI line
    pub params_to_leave: Vec<u32>,
    pub param_defaults: Vec<CliParamDefault>,
}
// pub fn dump_json(opts: &Opts) -> Result<CompilationTermination> {
//     let input =
//         opts.input.canonicalize().with_context(|| format!("failed to resolve {}", opts.input))?;
//     let input = AbsPathBuf::assert(input);
//     let db = CompilationDB::new_fs(input, &opts.include, &opts.defines, &opts.lints)?;
//     let modules = if let Some(modules) = collect_modules(&db, true, &mut ConsoleSink::new(&db)) {
//         modules
//     } else {
//         return Ok(CompilationTermination::FatalDiagnostic);
//     };
//     for module in modules {
//         let (func, intern, strings, cfg) = module.build_opvar_mir(&db);
//         let json = func.to_json(
//             &cfg,
//             &strings,
//             |param| match *intern.params.get_index(param).unwrap().0 {
//                 ParamKind::Param(param) => ("parameters", param.name(&db)),
//                 ParamKind::Abstime => ("sim_state", "$abstime".to_owned()),
//                 ParamKind::EnableIntegration => todo!(),
//                 ParamKind::Voltage { hi, lo: Some(lo) } => {
//                     ("voltages", format!("({}, {})", &hi.name(&db), &lo.name(&db)))
//                 }
//                 ParamKind::Voltage { hi, lo: None } => ("voltages", format!("({})", &hi.name(&db))),
//                 ParamKind::Current(hir_lower::CurrentKind::Unnamed { hi, lo: Some(lo) }) => {
//                     ("currents", format!("({}, {})", &hi.name(&db), &lo.name(&db)))
//                 }
//                 ParamKind::Current(hir_lower::CurrentKind::Unnamed { hi, lo: None }) => {
//                     ("currents", format!("({})", hi.name(&db)))
//                 }
//                 ParamKind::Current(hir_lower::CurrentKind::Branch(br)) => {
//                     ("currents", br.name(&db))
//                 }
//                 ParamKind::Temperature => ("sim_state", "$temperature".to_owned()),
//                 ParamKind::ParamGiven { param } => ("param_given", param.name(&db)),
//                 ParamKind::PortConnected { port } => ("port_connected", port.name(&db).to_string()),
//                 ParamKind::ParamSysFun(param) => ("params", format!("${param:?}")),
//                 _ => unreachable!(),
//             },
//             intern.outputs.iter().filter_map(|(kind, val)| {
//                 let name = match *kind {
//                     PlaceKind::Var(var) => var.name(&db).to_string(),
//                     _ => return None,
//                 };
//                 Some((name, val.expand()?))
//             }),
//         );
//         let path = opts.input.with_file_name(format!(
//             "{}_{}.json",
//             opts.input.file_stem().unwrap(),
//             module.module.name(&db)
//         ));
//         if !opts.dry_run {
//             std::fs::write(path, json)?;
//         }
//     }
//     Ok(CompilationTermination::Compiled { lib_file: Utf8PathBuf::default() })
// }

pub fn expand(opts: &Opts) -> Result<CompilationTermination> {
    let start = Instant::now();

    let input =
        opts.input.canonicalize().with_context(|| format!("failed to resolve {}", opts.input))?;
    let input = AbsPathBuf::assert(input);
    let db = CompilationDB::new_fs(
        input,
        &opts.include,
        &opts.defines,
        &opts.lints,
        &opts.param_defaults,
    )?;
    let cu = db.compilation_unit();

    let preprocess = cu.preprocess(&db);
    for token in preprocess.ts.iter() {
        let span = token.span.to_file_span(&preprocess.sm);
        let text = db.file_text(span.file).unwrap();
        match token.kind {
            tokens::parser::SyntaxKind::COMMENT => {
                // Block comments are ok
                // Line comments should be dumped with a newline
                if !text[span.range].starts_with("/*") {
                    println!("{}", &text[span.range])
                } else {
                    print!("{}", &text[span.range])
                }
            }
            _ => {
                // Add a space after each token
                print!("{} ", &text[span.range])
            }
        };
    }
    println!();

    let mut sink = ConsoleSink::new(&db);
    sink.add_diagnostics(&*preprocess.diagnostics, cu.root_file(), &db);

    if sink.summary(&opts.input.file_name().unwrap()) {
        return Ok(CompilationTermination::FatalDiagnostic);
    }

    let seconds = Instant::elapsed(&start).as_secs_f64();
    let mut stderr = StandardStream::stderr(ColorChoice::Auto);
    stderr.set_color(ColorSpec::new().set_fg(Some(Color::Green)).set_bold(true))?;
    write!(&mut stderr, "Finished")?;
    stderr.set_color(&ColorSpec::new())?;
    writeln!(&mut stderr, " preprocessing {} in {:.2}s", opts.input.file_name().unwrap(), seconds)?;

    Ok(CompilationTermination::Compiled { lib_file: Utf8PathBuf::default() })
}

pub fn compile(opts: &Opts) -> Result<CompilationTermination> {
    let start = Instant::now();

    let input =
        opts.input.canonicalize().with_context(|| format!("failed to resolve {}", opts.input))?;
    let input = AbsPathBuf::assert(input);
    let db = CompilationDB::new_fs(
        input,
        &opts.include,
        &opts.defines,
        &opts.lints,
        &opts.param_defaults,
    )?;

    let lib_file = match &opts.output {
        CompilationDestination::Cache { cache_dir } => {
            let file_name = cache::file_name(&db, opts);
            let lib_file = cache_dir.join(file_name);
            if cfg!(not(debug_assertions)) && lib_file.exists() {
                return Ok(CompilationTermination::Compiled { lib_file });
            }
            create_dir_all(cache_dir).context("failed to create cache directory")?;
            lib_file
        }
        CompilationDestination::Path { lib_file } => lib_file.clone(),
    };

    let modules = if let Some(modules) = collect_modules(&db, false, &mut ConsoleSink::new(&db)) {
        modules
    } else {
        return Ok(CompilationTermination::FatalDiagnostic);
    };

    if opts.dry_run {
        return Ok(CompilationTermination::Compiled { lib_file });
    }
    match opts.backend {
        CodegenBackend::Llvm => compile_with_llvm(opts, &db, &modules, &lib_file)?,
        CodegenBackend::MirLift => compile_with_mir_lift(opts, &db, &modules, &lib_file)?,
    }

    let seconds = Instant::elapsed(&start).as_secs_f64();
    let mut stderr = StandardStream::stderr(ColorChoice::Auto);
    stderr.set_color(ColorSpec::new().set_fg(Some(Color::Green)).set_bold(true))?;
    write!(&mut stderr, "Finished")?;
    stderr.set_color(&ColorSpec::new())?;
    writeln!(&mut stderr, " building {} in {:.2}s", opts.input.file_name().unwrap(), seconds)?;

    Ok(CompilationTermination::Compiled { lib_file })
}

fn compile_with_llvm(
    opts: &Opts,
    db: &CompilationDB,
    modules: &[sim_back::ModuleInfo],
    lib_file: &Utf8PathBuf,
) -> Result<()> {
    let back = LLVMBackend::new(&opts.codegen_opts, &opts.target, opts.target_cpu.clone(), &[]);
    let (paths, compiled_modules, literals) = osdi::compile(
        db,
        modules,
        lib_file,
        &opts.target,
        &back,
        true,
        opts.opt_lvl,
        opts.dump_mir,
        opts.dump_unopt_mir,
        opts.dump_ir,
        opts.dump_unopt_ir,
        &opts.params_to_leave,
    );

    if opts.dump_mir || opts.dump_unopt_mir {
        let cu = db.compilation_unit();
        println!("Compilation unit: {}", cu.name(db));
        println!("");

        println!("Literals:");
        for (k, v) in literals.iter() {
            println!("  {:?} -> '{}'", k, v);
        }
        println!("");

        for (module, cmodule) in modules.iter().zip(compiled_modules.iter()) {
            print_module("  ", db, module, &cmodule.dae_system, &cmodule.init);
            println!("");

            println!("Model setup HIR interner of {}", module.module.name(db));
            print_intern("  ", db, &cmodule.model_param_intern);
            println!("");

            println!("Instance setup HIR interner of {}", module.module.name(db));
            print_intern("  ", db, &cmodule.init.intern);
            println!("");

            println!("Evaluation HIR interner of {}", module.module.name(db));
            print_intern("  ", db, &cmodule.intern);
            println!("");
        }
    }

    link(None, &opts.target, lib_file.as_ref(), |linker| {
        for path in &paths {
            linker.add_object(path);
        }
    })?;

    for obj_file in paths {
        remove_file(obj_file).context("failed to delete intermediate compile artifact")?;
    }

    Ok(())
}

fn compile_with_mir_lift(
    opts: &Opts,
    db: &CompilationDB,
    modules: &[sim_back::ModuleInfo],
    lib_file: &Utf8PathBuf,
) -> Result<()> {
    let mut literals = Rodeo::default();
    let mut python = String::new();
    let mut first_unit = true;
    let mut stderr = StandardStream::stderr(ColorChoice::Auto);

    for module in modules {
        writeln!(&mut stderr, "mir-lift: building MIR for {}", module.module.name(db))?;
        let mut compiled = sim_back::CompiledModule::new(
            db,
            module,
            &mut literals,
            opts.dump_unopt_mir,
            opts.dump_mir,
            &opts.params_to_leave,
        );

        let base_name = module.module.name(db);
        compiled.model_param_setup.name = format!("_{base_name}_model_raw");
        compiled.init.func.name = format!("_{base_name}_init_raw");
        compiled.eval.name = format!("_{base_name}_eval_raw");
        let model_output_values = compiled
            .model_param_intern
            .outputs
            .values()
            .copied()
            .filter_map(|value| value.expand())
            .collect::<Vec<_>>();
        let init_output_values = compiled
            .init
            .intern
            .outputs
            .values()
            .copied()
            .filter_map(|value| value.expand())
            .collect::<Vec<_>>();
        let init_capture_values = compiled.init.cached_vals.keys().copied().collect::<Vec<_>>();
        let python_osdi = PythonOsdiModule::new(base_name.to_string(), db, &compiled);
        let eval_output_values = python_osdi.return_values();

        writeln!(&mut stderr, "mir-lift: lifting {} model setup", base_name)?;
        if opts.dump_lir {
            writeln!(&mut stderr, "mir-lift: LIR for {} model setup", base_name)?;
            writeln!(
                &mut stderr,
                "{}",
                mir_lift::dump_function_lir_with_hir_returns(
                    &compiled.model_param_setup,
                    &model_output_values,
                    &literals,
                    &compiled.model_param_intern,
                )?
            )?;
        }
        append_lifted_python(
            &mut python,
            &mir_lift::lift_function_with_hir_returns(
                &compiled.model_param_setup,
                &model_output_values,
                &literals,
                &compiled.model_param_intern,
            )?,
            &mut first_unit,
        )?;
        writeln!(&mut stderr, "mir-lift: lifting {} init", base_name)?;
        if opts.dump_lir {
            writeln!(&mut stderr, "mir-lift: LIR for {} init", base_name)?;
            writeln!(
                &mut stderr,
                "{}",
                mir_lift::dump_function_lir_with_hir_returns_and_captures(
                    &compiled.init.func,
                    &init_output_values,
                    &init_capture_values,
                    &literals,
                    &compiled.init.intern,
                )?
            )?;
        }
        append_lifted_python(
            &mut python,
            &mir_lift::lift_function_with_hir_returns_and_captures(
                &compiled.init.func,
                &init_output_values,
                &init_capture_values,
                &literals,
                &compiled.init.intern,
            )?,
            &mut first_unit,
        )?;
        writeln!(&mut stderr, "mir-lift: lifting {} eval", base_name)?;
        if opts.dump_lir {
            writeln!(&mut stderr, "mir-lift: LIR for {} eval", base_name)?;
            writeln!(
                &mut stderr,
                "{}",
                mir_lift::dump_function_lir_with_hir_returns(
                    &compiled.eval,
                    &eval_output_values,
                    &literals,
                    &compiled.intern,
                )?
            )?;
        }
        append_lifted_python(
            &mut python,
            &mir_lift::lift_function_with_hir_returns(
                &compiled.eval,
                &eval_output_values,
                &literals,
                &compiled.intern,
            )?,
            &mut first_unit,
        )?;
        append_python_osdi_module(&mut python, &python_osdi);
    }

    writeln!(&mut stderr, "mir-lift: writing {}", lib_file)?;
    std::fs::write(lib_file, python).context("failed to write mir_lift output")?;
    Ok(())
}

struct PythonOsdiModule {
    name: String,
    raw_model_function: String,
    raw_init_function: String,
    raw_eval_function: String,
    setup_model_function: String,
    setup_instance_function: String,
    eval_function: String,
    model_args: Vec<String>,
    init_args: Vec<String>,
    eval_args: Vec<String>,
    model_params: Vec<(String, mir::Value)>,
    model_instance_defaults: Vec<(String, mir::Value)>,
    instance_params: Vec<(String, mir::Value)>,
    cache_values: Vec<Option<mir::Value>>,
    builtin_params: Vec<(String, f64)>,
    hidden_slots: Vec<String>,
    init_hidden_values: Vec<(String, mir::Value)>,
    eval_hidden_values: Vec<(String, mir::Value)>,
    num_states: usize,
    num_cache_slots: usize,
    outputs: Vec<PythonOsdiOutputGroup>,
}

struct PythonOsdiOutputGroup {
    name: &'static str,
    values: Vec<Option<mir::Value>>,
}

impl PythonOsdiModule {
    fn new(name: String, db: &CompilationDB, compiled: &sim_back::CompiledModule<'_>) -> Self {
        let eval = &compiled.eval;
        Self {
            raw_model_function: sanitize_python_ident(&format!("_{name}_model_raw")),
            raw_init_function: sanitize_python_ident(&format!("_{name}_init_raw")),
            raw_eval_function: sanitize_python_ident(&format!("_{name}_eval_raw")),
            setup_model_function: sanitize_python_ident(&format!("{name}_setup_model")),
            setup_instance_function: sanitize_python_ident(&format!("{name}_setup_instance")),
            eval_function: sanitize_python_ident(&format!("{name}_eval")),
            model_args: python_osdi_setup_args(
                db,
                compiled,
                &compiled.model_param_intern,
                SetupPhase::Model,
            ),
            init_args: python_osdi_setup_args(
                db,
                compiled,
                &compiled.init.intern,
                SetupPhase::Instance,
            ),
            eval_args: python_osdi_eval_args(db, compiled),
            model_params: python_osdi_param_outputs(
                db,
                compiled,
                &compiled.model_param_intern,
                false,
            ),
            model_instance_defaults: python_osdi_param_outputs(
                db,
                compiled,
                &compiled.model_param_intern,
                true,
            ),
            instance_params: python_osdi_param_outputs(db, compiled, &compiled.init.intern, true),
            cache_values: python_osdi_cache_values(compiled),
            builtin_params: python_osdi_builtin_params(compiled),
            hidden_slots: python_osdi_hidden_slots(db, compiled),
            init_hidden_values: python_osdi_hidden_outputs(db, &compiled.init.intern),
            eval_hidden_values: python_osdi_hidden_outputs(db, &compiled.intern),
            num_states: compiled.intern.lim_state.len(),
            num_cache_slots: compiled.init.cache_slots.len(),
            outputs: vec![
                python_osdi_group(
                    "residual_resist",
                    compiled.dae_system.residual.iter().map(|residual| residual.resist),
                    eval,
                ),
                python_osdi_group(
                    "residual_react",
                    compiled.dae_system.residual.iter().map(|residual| residual.react),
                    eval,
                ),
                python_osdi_group(
                    "limit_rhs_resist",
                    compiled.dae_system.residual.iter().map(|residual| residual.resist_lim_rhs),
                    eval,
                ),
                python_osdi_group(
                    "limit_rhs_react",
                    compiled.dae_system.residual.iter().map(|residual| residual.react_lim_rhs),
                    eval,
                ),
                python_osdi_group(
                    "jacobian_resist",
                    compiled.dae_system.jacobian.iter().map(|entry| entry.resist),
                    eval,
                ),
                python_osdi_group(
                    "jacobian_react",
                    compiled.dae_system.jacobian.iter().map(|entry| entry.react),
                    eval,
                ),
            ],
            name,
        }
    }

    fn return_values(&self) -> Vec<mir::Value> {
        let mut seen = std::collections::HashSet::new();
        let mut values = Vec::new();
        for value in self.outputs.iter().flat_map(|group| group.values.iter().copied().flatten()) {
            if seen.insert(value) {
                values.push(value);
            }
        }
        for value in self.eval_hidden_values.iter().map(|(_, value)| *value) {
            if seen.insert(value) {
                values.push(value);
            }
        }
        values
    }
}

#[derive(Clone, Copy)]
enum SetupPhase {
    Model,
    Instance,
}

fn python_osdi_setup_args(
    db: &CompilationDB,
    compiled: &sim_back::CompiledModule<'_>,
    intern: &hir_lower::HirInterner,
    phase: SetupPhase,
) -> Vec<String> {
    let mut params: Vec<(usize, mir::Param)> = match phase {
        SetupPhase::Model => &compiled.model_param_setup,
        SetupPhase::Instance => &compiled.init.func,
    }
    .dfg
    .values()
    .filter_map(|value| {
        match match phase {
            SetupPhase::Model => compiled.model_param_setup.dfg.value_def(value),
            SetupPhase::Instance => compiled.init.func.dfg.value_def(value),
        } {
            mir::ValueDef::Param(param) => Some((usize::from(param), param)),
            _ => None,
        }
    })
    .collect();
    params.sort_by_key(|(index, _)| *index);

    params
        .into_iter()
        .map(|(_, param)| {
            intern.params.get_index(param).map_or("0.0".to_owned(), |(kind, _)| {
                python_osdi_setup_param_expr(db, compiled, kind, phase)
            })
        })
        .collect()
}

fn python_osdi_setup_param_expr(
    db: &CompilationDB,
    compiled: &sim_back::CompiledModule<'_>,
    kind: &hir_lower::ParamKind,
    phase: SetupPhase,
) -> String {
    match *kind {
        hir_lower::ParamKind::Param(param) => match phase {
            SetupPhase::Model => {
                format!("_pyosdi_dict_get(params, {:?}, 0.0)", param.name(db).to_string())
            }
            SetupPhase::Instance => {
                format!("_pyosdi_param(instance, model, {:?}, 0.0)", param.name(db).to_string())
            }
        },
        hir_lower::ParamKind::ParamGiven { param } => match phase {
            SetupPhase::Model => {
                format!("bool(_pyosdi_dict_get(given, {:?}, False))", param.name(db).to_string())
            }
            SetupPhase::Instance => {
                format!("_pyosdi_given(instance, model, {:?})", param.name(db).to_string())
            }
        },
        hir_lower::ParamKind::ParamSysFun(param) => match phase {
            SetupPhase::Model => {
                format!(
                    "_pyosdi_dict_get(builtin_params, {:?}, {:?})",
                    format!("{param:?}"),
                    param.default_value()
                )
            }
            SetupPhase::Instance => {
                format!(
                    "_pyosdi_builtin(instance, {:?}, {:?})",
                    format!("{param:?}"),
                    param.default_value()
                )
            }
        },
        hir_lower::ParamKind::Temperature => "float(temperature)".to_owned(),
        hir_lower::ParamKind::PortConnected { port } => {
            let index = compiled
                .dae_system
                .unknowns
                .index(&sim_back::SimUnknownKind::KirchoffLaw(port))
                .map(usize::from);
            format!(
                "_pyosdi_connected({{\"connected_terminals\": connected_terminals}}, {index:?})"
            )
        }
        hir_lower::ParamKind::Voltage { .. }
        | hir_lower::ParamKind::Current(_)
        | hir_lower::ParamKind::ImplicitUnknown(_)
        | hir_lower::ParamKind::Abstime
        | hir_lower::ParamKind::EnableIntegration
        | hir_lower::ParamKind::EnableLim
        | hir_lower::ParamKind::PrevState(_)
        | hir_lower::ParamKind::NewState(_) => "0.0".to_owned(),
        hir_lower::ParamKind::HiddenState(var) => match phase {
            SetupPhase::Model => format!("_pyosdi_missing_hidden({:?})", var.name(db).to_string()),
            SetupPhase::Instance => {
                format!("_pyosdi_hidden(instance, {:?})", var.name(db).to_string())
            }
        },
    }
}

fn python_osdi_param_outputs(
    db: &CompilationDB,
    compiled: &sim_back::CompiledModule<'_>,
    intern: &hir_lower::HirInterner,
    want_instance: bool,
) -> Vec<(String, mir::Value)> {
    intern
        .outputs
        .iter()
        .filter_map(|(kind, value)| match kind {
            hir_lower::PlaceKind::Param(param) => {
                if compiled.info.params[param].is_instance != want_instance {
                    return None;
                }
                Some((param.name(db).to_string(), value.expand()?))
            }
            _ => None,
        })
        .collect()
}

fn python_osdi_cache_values(compiled: &sim_back::CompiledModule<'_>) -> Vec<Option<mir::Value>> {
    let mut cache_values = vec![None; compiled.init.cache_slots.len()];
    for (&value, &slot) in compiled.init.cached_vals.iter() {
        cache_values[usize::from(slot)] = Some(value);
    }
    cache_values
}

fn python_osdi_eval_args(
    db: &CompilationDB,
    compiled: &sim_back::CompiledModule<'_>,
) -> Vec<String> {
    let mut params: Vec<(usize, mir::Param)> = compiled
        .eval
        .dfg
        .values()
        .filter_map(|value| match compiled.eval.dfg.value_def(value) {
            mir::ValueDef::Param(param) => Some((usize::from(param), param)),
            _ => None,
        })
        .collect();
    params.sort_by_key(|(index, _)| *index);

    params
        .into_iter()
        .map(|(index, param)| {
            if let Some((kind, _)) = compiled.intern.params.get_index(param) {
                python_osdi_param_expr(db, compiled, kind)
            } else {
                let cache_index = index.saturating_sub(compiled.intern.params.len());
                format!("_pyosdi_get(instance.get(\"cache\", []), {cache_index}, 0.0)")
            }
        })
        .collect()
}

fn python_osdi_builtin_params(compiled: &sim_back::CompiledModule<'_>) -> Vec<(String, f64)> {
    compiled
        .intern
        .params
        .iter()
        .filter_map(|(kind, _)| match kind {
            hir_lower::ParamKind::ParamSysFun(param) => {
                Some((format!("{param:?}"), param.default_value()))
            }
            _ => None,
        })
        .collect()
}

fn python_osdi_hidden_slots(
    db: &CompilationDB,
    compiled: &sim_back::CompiledModule<'_>,
) -> Vec<String> {
    let mut slots = std::collections::BTreeSet::new();
    python_osdi_collect_hidden_state_refs(
        db,
        &compiled.model_param_setup,
        &compiled.model_param_intern,
        &mut slots,
    );
    python_osdi_collect_hidden_state_refs(
        db,
        &compiled.init.func,
        &compiled.init.intern,
        &mut slots,
    );
    python_osdi_collect_hidden_state_refs(db, &compiled.eval, &compiled.intern, &mut slots);
    for (name, _) in python_osdi_hidden_outputs(db, &compiled.init.intern) {
        slots.insert(name);
    }
    for (name, _) in python_osdi_hidden_outputs(db, &compiled.intern) {
        slots.insert(name);
    }
    slots.into_iter().collect()
}

fn python_osdi_collect_hidden_state_refs(
    db: &CompilationDB,
    func: &mir::Function,
    intern: &hir_lower::HirInterner,
    slots: &mut std::collections::BTreeSet<String>,
) {
    let params = mir_lift::collect_live_param_refs_forward(func)
        .expect("compiled MIR function used for Python OSDI hidden-state discovery must be valid");
    for param in params {
        if let Some((hir_lower::ParamKind::HiddenState(var), _)) = intern.params.get_index(param) {
            slots.insert(var.name(db).to_string());
        }
    }
}

fn python_osdi_hidden_outputs(
    db: &CompilationDB,
    intern: &hir_lower::HirInterner,
) -> Vec<(String, mir::Value)> {
    intern
        .outputs
        .iter()
        .filter_map(|(kind, value)| match kind {
            hir_lower::PlaceKind::Var(var) => Some((var.name(db).to_string(), value.expand()?)),
            _ => None,
        })
        .collect()
}

fn python_osdi_param_expr(
    db: &CompilationDB,
    compiled: &sim_back::CompiledModule<'_>,
    kind: &hir_lower::ParamKind,
) -> String {
    match *kind {
        hir_lower::ParamKind::Param(param) => {
            format!("_pyosdi_param(instance, model, {:?}, 0.0)", param.name(db).to_string())
        }
        hir_lower::ParamKind::Abstime => "float(sim_info.get(\"abstime\", 0.0))".to_owned(),
        hir_lower::ParamKind::EnableIntegration => {
            "((int(sim_info.get(\"flags\", 0)) & 8) != 0 and (int(sim_info.get(\"flags\", 0)) & 16384) == 0)"
                .to_owned()
        }
        hir_lower::ParamKind::EnableLim => {
            "((int(sim_info.get(\"flags\", 0)) & 256) != 0)".to_owned()
        }
        hir_lower::ParamKind::PrevState(state) => {
            format!(
                "_pyosdi_state(sim_info, \"prev_state\", _pyosdi_state_idx(instance, {}))",
                usize::from(state)
            )
        }
        hir_lower::ParamKind::NewState(state) => {
            format!(
                "_pyosdi_state(sim_info, \"next_state\", _pyosdi_state_idx(instance, {}))",
                usize::from(state)
            )
        }
        hir_lower::ParamKind::Voltage { hi, lo } => {
            let hi = python_osdi_solve_expr(compiled, sim_back::SimUnknownKind::KirchoffLaw(hi));
            if let Some(lo) = lo {
                let lo = python_osdi_solve_expr(compiled, sim_back::SimUnknownKind::KirchoffLaw(lo));
                format!("({hi} - {lo})")
            } else {
                hi
            }
        }
        hir_lower::ParamKind::Current(hir_lower::CurrentKind::Port(_)) => "0.0".to_owned(),
        hir_lower::ParamKind::Current(kind) => {
            python_osdi_solve_expr(compiled, sim_back::SimUnknownKind::Current(kind))
        }
        hir_lower::ParamKind::Temperature => "float(instance.get(\"temperature\", 300.0))".to_owned(),
        hir_lower::ParamKind::ParamGiven { param } => {
            format!("_pyosdi_given(instance, model, {:?})", param.name(db).to_string())
        }
        hir_lower::ParamKind::PortConnected { port } => {
            let index = compiled
                .dae_system
                .unknowns
                .index(&sim_back::SimUnknownKind::KirchoffLaw(port))
                .map(usize::from);
            format!("_pyosdi_connected(sim_info, {index:?})")
        }
        hir_lower::ParamKind::ParamSysFun(param) => {
            format!("_pyosdi_builtin(instance, {:?}, {:?})", format!("{param:?}"), param.default_value())
        }
        hir_lower::ParamKind::HiddenState(var) => {
            format!("_pyosdi_hidden(instance, {:?})", var.name(db).to_string())
        }
        hir_lower::ParamKind::ImplicitUnknown(equation) => {
            python_osdi_solve_expr(compiled, sim_back::SimUnknownKind::Implicit(equation))
        }
    }
}

fn python_osdi_solve_expr(
    compiled: &sim_back::CompiledModule<'_>,
    unknown: sim_back::SimUnknownKind,
) -> String {
    match compiled.dae_system.unknowns.index(&unknown).map(usize::from) {
        Some(index) => format!("_pyosdi_solve(sim_info, {index})"),
        None => "0.0".to_owned(),
    }
}

fn python_osdi_group(
    name: &'static str,
    values: impl IntoIterator<Item = mir::Value>,
    eval: &mir::Function,
) -> PythonOsdiOutputGroup {
    PythonOsdiOutputGroup {
        name,
        values: values
            .into_iter()
            .map(|value| {
                let value = mir::strip_optbarrier(eval, value);
                if value == mir::F_ZERO {
                    None
                } else {
                    Some(value)
                }
            })
            .collect(),
    }
}

fn append_python_osdi_module(out: &mut String, module: &PythonOsdiModule) {
    out.push_str("\n\n");
    out.push_str(
        r#"def _pyosdi_get(seq, idx, default=0.0):
    try:
        return seq[idx]
    except Exception:
        return default


def _pyosdi_solve(sim_info, idx):
    if idx is None:
        return 0.0
    return _pyosdi_get(sim_info.get("prev_solve", []), idx, 0.0)


def _pyosdi_dict_get(items, key, default=0.0):
    if items is None:
        return default
    return items.get(key, default)


def _pyosdi_raw_get(items, key, default=0.0):
    val = items.get(key, default)
    if val is None:
        return default
    return val


def _pyosdi_state(sim_info, which, idx):
    return _pyosdi_get(sim_info.get(which, []), idx, 0.0)


def _pyosdi_state_idx(instance, idx):
    return _pyosdi_get(instance.get("state_idx", []), idx, idx)


def _pyosdi_param(instance, model, name, default=0.0):
    inst_params = instance.get("params", {})
    if name in inst_params:
        return inst_params[name]
    return model.get("params", {}).get(name, default)


def _pyosdi_given(instance, model, name):
    inst_given = instance.get("given", {})
    if name in inst_given and inst_given[name]:
        return True
    return bool(model.get("given", {}).get(name, False))


def _pyosdi_builtin(instance, name, default):
    return instance.get("builtin_params", {}).get(name, default)


def _pyosdi_connected(sim_info, idx):
    if idx is None:
        return False
    connected = sim_info.get("connected_terminals", sim_info.get("num_terminals", 0))
    return idx < connected


def _pyosdi_hidden(instance, name):
    hidden = instance.setdefault("hidden", {})
    if name not in hidden:
        hidden[name] = 0.0
    return hidden[name]


def _pyosdi_missing_hidden(name):
    raise RuntimeError(f"hidden state {name!r} is required before Python OSDI setup can run")

"#,
    );

    out.push_str(&format!(
        "def {}(params=None, given=None, builtin_params=None):\n",
        module.setup_model_function
    ));
    out.push_str("    if params is None:\n");
    out.push_str("        params = {}\n");
    out.push_str("    if given is None:\n");
    out.push_str("        given = {}\n");
    out.push_str("    if builtin_params is None:\n");
    out.push_str("        builtin_params = {}\n");
    out.push_str("    _raw_args = [\n");
    for arg in &module.model_args {
        out.push_str("        ");
        out.push_str(arg);
        out.push_str(",\n");
    }
    out.push_str("    ]\n");
    out.push_str(&format!("    _raw = {}(*_raw_args)\n", module.raw_model_function));
    out.push_str("    _params = dict(params)\n");
    out.push_str("    _inst_defaults = {}\n");
    for (name, value) in &module.model_params {
        out.push_str(&format!(
            "    _params[{name:?}] = _pyosdi_raw_get(_raw, {:?}, _params.get({name:?}, 0.0))\n",
            value.to_string()
        ));
    }
    for (name, value) in &module.model_instance_defaults {
        out.push_str(&format!(
            "    _inst_defaults[{name:?}] = _pyosdi_raw_get(_raw, {:?}, _pyosdi_dict_get(params, {name:?}, 0.0))\n",
            value.to_string()
        ));
    }
    out.push_str(&format!(
        "    return {{\"module\": {:?}, \"raw\": _raw, \"params\": _params, \"given\": dict(given), \"inst_defaults\": _inst_defaults, \"builtin_params\": dict(builtin_params)}}\n",
        module.name
    ));

    out.push_str("\n\n");
    out.push_str(&format!(
        "def {}(model=None, temperature=300.0, params=None, given=None, connected_terminals=0):\n",
        module.setup_instance_function
    ));
    out.push_str("    if model is None:\n");
    out.push_str(&format!("        model = {}()\n", module.setup_model_function));
    out.push_str("    _builtin_params = {");
    for (idx, (name, default)) in module.builtin_params.iter().enumerate() {
        if idx != 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{name:?}: {default:?}"));
    }
    out.push_str("}\n");
    out.push_str("    _params = dict(model.get(\"inst_defaults\", {}))\n");
    out.push_str("    if params:\n");
    out.push_str("        _params.update(params)\n");
    out.push_str("    _given = dict(given or {})\n");
    out.push_str("    _hidden = {");
    for (idx, name) in module.hidden_slots.iter().enumerate() {
        if idx != 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{name:?}: 0.0"));
    }
    out.push_str("}\n");
    out.push_str("    instance = {\"model\": model, \"temperature\": temperature, \"raw\": {}, \"outputs\": {}, \"params\": _params, \"given\": _given, \"builtin_params\": _builtin_params, \"hidden\": _hidden, \"state_idx\": list(range(");
    out.push_str(&module.num_states.to_string());
    out.push_str(")), \"cache\": [0.0] * ");
    out.push_str(&module.num_cache_slots.to_string());
    out.push_str("}\n");
    out.push_str("    _raw_args = [\n");
    for arg in &module.init_args {
        out.push_str("        ");
        out.push_str(arg);
        out.push_str(",\n");
    }
    out.push_str("    ]\n");
    out.push_str(&format!("    _raw = {}(*_raw_args)\n", module.raw_init_function));
    out.push_str("    instance[\"raw\"] = _raw\n");
    for (name, value) in &module.instance_params {
        out.push_str(&format!(
            "    instance[\"params\"][{name:?}] = _pyosdi_raw_get(_raw, {:?}, instance[\"params\"].get({name:?}, 0.0))\n",
            value.to_string()
        ));
    }
    out.push_str("    _cache = instance[\"cache\"]\n");
    for (idx, value) in module.cache_values.iter().enumerate() {
        if let Some(value) = value {
            out.push_str(&format!(
                "    _cache[{idx}] = _pyosdi_raw_get(_raw, {:?}, _cache[{idx}])\n",
                value.to_string()
            ));
        }
    }
    out.push_str("    _hidden = instance[\"hidden\"]\n");
    for (name, value) in &module.init_hidden_values {
        out.push_str(&format!(
            "    _hidden[{name:?}] = _pyosdi_raw_get(_raw, {:?}, _hidden.get({name:?}, 0.0))\n",
            value.to_string()
        ));
    }
    out.push_str(&format!("    return instance\n"));

    out.push_str("\n\n");
    out.push_str(&format!(
        "def {}(instance=None, model=None, sim_info=None):\n",
        module.eval_function
    ));
    out.push_str("    if instance is None:\n");
    out.push_str(&format!("        instance = {}(model)\n", module.setup_instance_function));
    out.push_str("    if model is None:\n");
    out.push_str("        model = instance.get(\"model\", {})\n");
    out.push_str("    if sim_info is None:\n");
    out.push_str("        sim_info = {}\n");
    out.push_str("    _raw_args = [\n");
    for arg in &module.eval_args {
        out.push_str("        ");
        out.push_str(arg);
        out.push_str(",\n");
    }
    out.push_str("    ]\n");
    out.push_str("    _raw = ");
    out.push_str(&module.raw_eval_function);
    out.push_str("(*_raw_args)\n");
    out.push_str("    _outputs = {\n");
    for group in &module.outputs {
        out.push_str(&format!("        {:?}: [", group.name));
        for (idx, value) in group.values.iter().enumerate() {
            if idx != 0 {
                out.push_str(", ");
            }
            match value {
                Some(value) => out.push_str(&format!("_raw[{:?}]", value.to_string())),
                None => out.push_str("0.0"),
            }
        }
        out.push_str("],\n");
    }
    out.push_str("    }\n");
    out.push_str("    instance[\"outputs\"] = _outputs\n");
    out.push_str("    _hidden = instance.setdefault(\"hidden\", {})\n");
    for (name, value) in &module.eval_hidden_values {
        out.push_str(&format!(
            "    _hidden[{name:?}] = _pyosdi_raw_get(_raw, {:?}, _hidden.get({name:?}, 0.0))\n",
            value.to_string()
        ));
    }
    out.push_str("    return {\"flags\": 0, **_outputs}\n");

    for group in &module.outputs {
        out.push_str("\n\n");
        out.push_str(&format!("def {0}_load_{1}(instance):\n", module.name, group.name));
        out.push_str(&format!(
            "    return instance.get(\"outputs\", {{}}).get({:?}, [])\n",
            group.name
        ));
    }
}

fn sanitize_python_ident(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.as_bytes().first().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn append_lifted_python(out: &mut String, lifted: &str, first_unit: &mut bool) -> Result<()> {
    if *first_unit {
        out.push_str(lifted);
        *first_unit = false;
        return Ok(());
    }

    out.push('\n');
    out.push('\n');
    out.push_str(strip_lift_prelude(lifted));
    Ok(())
}

fn strip_lift_prelude(lifted: &str) -> &str {
    lifted.find("\ndef ").map(|idx| &lifted[idx + 1..]).unwrap_or(lifted)
}
