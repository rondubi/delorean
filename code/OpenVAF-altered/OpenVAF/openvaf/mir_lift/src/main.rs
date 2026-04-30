use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args = Args::parse()?;
    let input = fs::read_to_string(&args.input)
        .with_context(|| format!("failed to read {}", args.input.display()))?;
    let metadata = match &args.compilation_db {
        Some(path) => Some(
            fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        ),
        None => None,
    };

    let output = if args.dump_lir {
        mir_lift::dump_lir_text(&input, metadata.as_deref())?
    } else {
        mir_lift::lift_text(&input, metadata.as_deref())?
    };
    fs::write(&args.output, output)
        .with_context(|| format!("failed to write {}", args.output.display()))?;
    Ok(())
}

struct Args {
    input: PathBuf,
    output: PathBuf,
    compilation_db: Option<PathBuf>,
    dump_lir: bool,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args_os().skip(1);
        let input = args.next().map(PathBuf::from).context(
            "usage: mir_lift <input.mir> -o <output.py|output.lir> [--compilation-db <metadata.json>] [--dump-lir]",
        )?;
        let mut output = None;
        let mut compilation_db = None;
        let mut dump_lir = false;

        while let Some(arg) = args.next() {
            match arg.to_string_lossy().as_ref() {
                "-o" | "--output" => {
                    output =
                        Some(args.next().map(PathBuf::from).context("missing value for --output")?);
                }
                "--compilation-db" => {
                    compilation_db = Some(
                        args.next()
                            .map(PathBuf::from)
                            .context("missing value for --compilation-db")?,
                    );
                }
                "--dump-lir" => {
                    dump_lir = true;
                }
                other => bail!("unrecognized argument: {other}"),
            }
        }

        Ok(Self {
            input,
            output: output.context("missing required --output")?,
            compilation_db,
            dump_lir,
        })
    }
}
