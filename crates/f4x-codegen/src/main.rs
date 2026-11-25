//! `f4x-codegen <subcommand>`; see `USAGE`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use f4x_codegen::flavor::Flavor;
use f4x_codegen::{dump, records, tables, workspace_root};

const USAGE: &str = "\
usage: f4x-codegen <command> [options]

commands:
  dump      run clang over CommonLibF4; write target/clangdump/{ast.json, records.txt,
            vtables.txt, layouts.json, relocs.json, dump-info.json}
  layouts   parse <out>/records.txt into <out>/layouts.json
  tables    generate crates/f4x-engine-sys/src/og_1_10_163.rs (--flavor ng: ng_1_11_240.rs)
            (offset tables, vtable slots, RTTI names, form types) from
            <out>/{layouts.json, vtables.txt}
  addresses resolve crates/f4x-engine-sys/wanted.txt with --bin <Address Library .bin>
            and --exe <Fallout4.exe>; write signature recipes into f4x-engine-sys
            (--flavor ng: no bin; --exe <1.11.240 Fallout4.exe> and the RVAs of
            wanted-ng_1_11_240.txt -> ng_1_11_240_addresses.rs)
  verify    check the generated tables against a game exe: --exe <Fallout4.exe>
            (vtable lengths vs RTTI, classes present); writes <out>/verify.json;
            --flavor ng also rewrites crates/f4x-codegen/ng-rtti-keys.txt (RTTI
            class-keys the exe spells differently from the headers; rerun tables)

options:
  --flavor og|ng        og (default): CommonLibF4 for 1.10.163 -> og_1_10_163.rs;
                        ng: libxse/commonlibf4 40a93f69 with our REL/REX shims
                        (crates/f4x-codegen/shim-ng) -> ng_1_11_240.rs, dump in
                        target/clangdump-ng, OG-name aliases from ng-aliases.txt
  --clf4 <dir>          CommonLibF4 checkout (default: <workspace>/../CommonLibF4;
                        ng: <workspace>/../commonlibf4-ng)
  --clang <path>        clang++ to use (default: $F4X_CLANG, else clang++ on PATH, skipping ccache)
  --xwin <dir>          MSVC CRT/SDK from cargo-xwin (default: ~/.cache/cargo-xwin/xwin)
  --out <dir>           output directory (default: <workspace>/target/clangdump[-ng])
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
    exe: Option<PathBuf>,
    bin: Option<PathBuf>,
    offline: bool,
    delayed_templates: bool,
    flavor: Flavor,
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
        exe: None,
        bin: None,
        offline: false,
        delayed_templates: true,
        flavor: Flavor::Og,
    };
    while let Some(flag) = it.next() {
        let mut value = || it.next().map(PathBuf::from).ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--clf4" => args.clf4 = Some(value()?),
            "--clang" => args.clang = Some(value()?),
            "--xwin" => args.xwin = Some(value()?),
            "--out" => args.out = Some(value()?),
            "--third-party" => args.third_party = Some(value()?),
            "--exe" => args.exe = Some(value()?),
            "--bin" => args.bin = Some(value()?),
            "--offline" => args.offline = true,
            "--no-delayed-templates" => args.delayed_templates = false,
            "--flavor" => args.flavor = Flavor::parse(&value()?.to_string_lossy())?,
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
    let out = args.out.clone().unwrap_or_else(|| args.flavor.default_out());
    let clf4 = args.clf4.clone().unwrap_or_else(|| args.flavor.default_clf4());
    let result = match args.command.as_str() {
        "dump" => {
            let opts = dump::Options {
                clf4: clf4.clone(),
                clang: args.clang.clone(),
                xwin: args.xwin.clone(),
                out,
                third_party: args
                    .third_party
                    .clone()
                    .unwrap_or_else(|| workspace_root().join("target/third_party")),
                offline: args.offline,
                delayed_templates: args.delayed_templates,
                flavor: args.flavor,
            };
            dump::run(&opts).and_then(|ok| if ok { Ok(()) } else { Err("clang reported errors".into()) })
        }
        "layouts" => layouts(&out),
        "tables" => tables(&out, &clf4, args.flavor),
        "addresses" => match (&args.bin, &args.exe) {
            (_, Some(exe)) if args.flavor == Flavor::Ng => f4x_codegen::addresses::run_ng(
                exe,
                &workspace_root().join("crates/f4x-engine-sys/wanted.txt"),
                &workspace_root().join("crates/f4x-engine-sys/wanted-ng_1_11_240.txt"),
                &workspace_root().join("crates/f4x-engine-sys/src/ng_1_11_240_addresses.rs"),
                &workspace_root().join("provenance/addresses-ng_1_11_240.jsonl"),
            ),
            (Some(bin), Some(exe)) => f4x_codegen::addresses::run(
                bin,
                exe,
                &workspace_root().join("crates/f4x-engine-sys/wanted.txt"),
                &workspace_root().join("crates/f4x-engine-sys/src/og_1_10_163_addresses.rs"),
                &workspace_root().join("provenance/addresses.jsonl"),
            ),
            _ => Err("addresses needs --bin <version-1-10-163-0.bin> and --exe <Fallout4.exe>".into()),
        },
        "verify" => match &args.exe {
            Some(exe) => verify(&out, exe, &clf4, args.flavor),
            None => Err("verify needs --exe <Fallout4.exe>".into()),
        },
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

fn tables(out: &std::path::Path, clf4: &std::path::Path, flavor: Flavor) -> Result<(), String> {
    let start = Instant::now();
    let inputs = tables::load(out, clf4, flavor)?;
    let aliases = match flavor.aliases_file() {
        Some(path) => tables::parse_aliases(
            &std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
        )?,
        None => Vec::new(),
    };
    let generated = tables::generate(&inputs, flavor, &aliases)?;
    let target = workspace_root().join(format!("crates/f4x-engine-sys/src/{}.rs", flavor.module()));
    std::fs::write(&target, &generated.source).map_err(|e| format!("{}: {e}", target.display()))?;
    println!(
        "tables: {} classes, {} vtables, {} form types, {} aliases, {} KiB in {:.2}s -> {}",
        generated.classes,
        generated.vtables,
        inputs.form_types.len(),
        generated.aliases,
        generated.source.len() / 1024,
        start.elapsed().as_secs_f64(),
        target.display()
    );
    Ok(())
}

fn verify(out: &std::path::Path, exe: &std::path::Path, clf4: &std::path::Path, flavor: Flavor) -> Result<(), String> {
    use f4x_pe::PeFile;
    use f4x_pe::rtti::{RttiIndex, vtable_len};
    let inputs = tables::load(out, clf4, flavor)?;
    let bytes = std::fs::read(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let pe = PeFile::parse(&bytes).map_err(|e| e.to_string())?;
    let index = RttiIndex::scan(&pe);
    let (mut matched, mut mismatched, mut absent, mut checked) = (0, Vec::new(), Vec::new(), 0);
    let mut sorted: Vec<_> = inputs.vtables.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (class, slots) in sorted {
        if class.contains('<') {
            continue;
        }
        let layout = inputs.layouts.iter().find(|l| l["name"].as_str() == Some(class.as_str()));
        let is_struct = layout.is_some_and(|l| l["kind"].as_str() == Some("struct"));
        let mangled = tables::rtti_name(class, is_struct);
        let alt = tables::rtti_name(class, !is_struct);
        let vtable = index.primary_vtable(&mangled).or_else(|| index.primary_vtable(&alt));
        checked += 1;
        match vtable {
            None => absent.push(class.clone()),
            Some(rva) => {
                let len = vtable_len(&pe, rva);
                if len == slots.len() {
                    matched += 1;
                } else {
                    mismatched.push(serde_json::json!({"class": class, "clf4": slots.len(), "exe": len}));
                }
            }
        }
    }
    let report = serde_json::json!({
        "exe": exe.display().to_string(), "checked": checked, "matched": matched,
        "mismatched": mismatched, "absent_in_exe": absent,
    });
    std::fs::write(out.join("verify.json"), serde_json::to_string_pretty(&report).unwrap_or_default()).map_err(|e| e.to_string())?;
    println!(
        "verify: {checked} polymorphic classes; {matched} vtable lengths match the exe, {} differ, {} have no vtable in the exe -> {}",
        mismatched.len(),
        absent.len(),
        out.join("verify.json").display()
    );
    for m in mismatched.iter().take(15) {
        println!("  differs: {} clf4={} exe={}", m["class"], m["clf4"], m["exe"]);
    }
    // Classes whose RTTI type descriptor in the exe uses the other class-key.
    let rtti_keys = tables::rtti_key_fixes(&inputs, flavor, |name| index.type_descriptors.contains_key(name));
    println!("verify: {} classes have the other RTTI class-key in the exe", rtti_keys.len());
    if let Some(path) = flavor.rtti_keys_file() {
        let name = exe.file_name().map_or_else(|| exe.display().to_string(), |n| n.to_string_lossy().into_owned());
        std::fs::write(&path, tables::render_rtti_keys(&rtti_keys, &name)).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("  -> {} (run `tables` again to apply)", path.display());
    }
    Ok(())
}
