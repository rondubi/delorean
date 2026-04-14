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
use sim_back::{print_module, print_intern};
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
    let db = CompilationDB::new_fs(input, &opts.include, &opts.defines, &opts.lints, &opts.param_defaults)?;
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
            }, 
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
    let db = CompilationDB::new_fs(input, &opts.include, &opts.defines, &opts.lints, &opts.param_defaults)?;

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
        writeln!(
            &mut stderr,
            "mir-lift: building MIR for {}",
            module.module.name(db)
        )?;
        let mut compiled = sim_back::CompiledModule::new(
            db,
            module,
            &mut literals,
            opts.dump_unopt_mir,
            opts.dump_mir,
            &opts.params_to_leave,
        );

        let base_name = module.module.name(db);
        compiled.model_param_setup.name = format!("{base_name}_model");
        compiled.init.func.name = format!("{base_name}_init");
        compiled.eval.name = format!("{base_name}_eval");
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
        let eval_output_values = compiled
            .intern
            .outputs
            .values()
            .copied()
            .filter_map(|value| value.expand())
            .collect::<Vec<_>>();

        writeln!(&mut stderr, "mir-lift: lifting {} model setup", base_name)?;
        append_lifted_python(
            &mut python,
            &mir_lift::lift_function_with_returns(
                &compiled.model_param_setup,
                &model_output_values,
                &literals,
            )?,
            &mut first_unit,
        )?;
        writeln!(&mut stderr, "mir-lift: lifting {} init", base_name)?;
        append_lifted_python(
            &mut python,
            &mir_lift::lift_function_with_returns(
                &compiled.init.func,
                &init_output_values,
                &literals,
            )?,
            &mut first_unit,
        )?;
        writeln!(&mut stderr, "mir-lift: lifting {} eval", base_name)?;
        append_lifted_python(
            &mut python,
            &mir_lift::lift_function_with_returns(
                &compiled.eval,
                &eval_output_values,
                &literals,
            )?,
            &mut first_unit,
        )?;
    }

    writeln!(&mut stderr, "mir-lift: writing {}", lib_file)?;
    std::fs::write(lib_file, python).context("failed to write mir_lift output")?;
    Ok(())
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
    lifted
        .find("\ndef ")
        .map(|idx| &lifted[idx + 1..])
        .unwrap_or(lifted)
}
