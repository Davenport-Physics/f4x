//! `f4x-codegen <subcommand>`; see `USAGE`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use f4x_codegen::{default_out_dir, dump, records, workspace_root};

const USAGE: &str = "\
usage: f4x-codegen <command> [options]

commands:
  dump      run clang over CommonLibF4; write target/clangdump/{ast.json, records.txt,
            vtables.txt, layouts.json, relocs.json, dump-info.json}
  layouts   parse <out>/records.txt into <out>/layouts.json

options:
  --clf4 <dir>          CommonLibF4 checkout (default: <workspace>/../CommonLibF4)
  --clang <path>        clang++ to use (default: $F4X_CLANG, else clang++ on PATH, skipping ccache)
  --xwin <dir>          MSVC CRT/SDK from cargo-xwin (default: ~/.cache/cargo-xwin/xwin)
  --out <dir>           output directory (default: <workspace>/target/clangdump)
  --third-party <dir>   where pinned header tarballs live (default: <workspace>/target/third_party)
  --offline             never download; fail if a pinned tarball is missing
  --no-delayed-templates
                        parse template bodies eagerly (strict two-phase lookup) instead of
                        MSVC-style; CommonLibF4 then has 2 latent errors in BSScriptUtil.h
";

struct Args {
    command: String,
    clf4: Option<PathBuf>,
    clang: Option<PathBuf>,
    xwin: Option<PathBuf>,
    out: Option<PathBuf>,
    third_party: Option<PathBuf>,
    offline: bool,
    delayed_templates: bool,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut it = raw.iter();
    let command = it.next().ok_or("missing command")?.clone();
    let mut args = Args {
        command,
        clf4: None,
        clang: None,
        xwin: None,
        out: None,
        third_party: None,
        offline: false,
        delayed_templates: true,
    };
    while let Some(flag) = it.next() {
        let mut value = || it.next().map(PathBuf::from).ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--clf4" => args.clf4 = Some(value()?),
            "--clang" => args.clang = Some(value()?),
            "--xwin" => args.xwin = Some(value()?),
            "--out" => args.out = Some(value()?),
            "--third-party" => args.third_party = Some(value()?),
            "--offline" => args.offline = true,
            "--no-delayed-templates" => args.delayed_templates = false,
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || matches!(raw[0].as_str(), "-h" | "--help" | "help") {
        print!("{USAGE}");
        return if raw.is_empty() { ExitCode::from(2) } else { ExitCode::SUCCESS };
    }
    let args = match parse_args(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let out = args.out.clone().unwrap_or_else(default_out_dir);
    let result = match args.command.as_str() {
        "dump" => {
            let opts = dump::Options {
                clf4: args.clf4.clone().unwrap_or_else(|| workspace_root().join("../CommonLibF4")),
                clang: args.clang.clone(),
                xwin: args.xwin.clone(),
                out,
                third_party: args
                    .third_party
                    .clone()
                    .unwrap_or_else(|| workspace_root().join("target/third_party")),
                offline: args.offline,
                delayed_templates: args.delayed_templates,
            };
            dump::run(&opts).and_then(|ok| if ok { Ok(()) } else { Err("clang reported errors".into()) })
        }
        "layouts" => layouts(&out),
        other => {
            eprintln!("error: unknown command `{other}`\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn layouts(out: &std::path::Path) -> Result<(), String> {
    let start = Instant::now();
    let input = out.join("records.txt");
    let text = std::fs::read_to_string(&input)
        .map_err(|e| format!("reading {} (run `f4x-codegen dump` first): {e}", input.display()))?;
    let parsed = records::parse(&text);
    let output = out.join("layouts.json");
    dump::write_layouts(&parsed.records, &output)?;
    let s = records::summarize(&parsed.records);
    println!(
        "layouts: {} records ({} unique names), {} RE:: ({} non-template, {} polymorphic), {} parse warnings, {:.2}s -> {}",
        s.records,
        s.unique_names,
        s.re_records,
        s.re_non_template,
        s.re_polymorphic,
        parsed.warnings.len(),
        start.elapsed().as_secs_f64(),
        output.display()
    );
    for w in parsed.warnings.iter().take(10) {
        eprintln!("warning: {w}");
    }
    Ok(())
}
