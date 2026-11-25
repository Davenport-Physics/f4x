//! `dump`: run clang over CommonLibF4 and write `target/clangdump/`.
//!
//! Passes (records and the two AST passes run in parallel; vtables follows records):
//!
//! | pass      | mode                                                    | output |
//! |-----------|---------------------------------------------------------|--------|
//! | records   | `-fsyntax-only -fdump-record-layouts-complete -canonical` | `records.txt`, `layouts.json` |
//! | ast-RE    | `-fsyntax-only -ast-dump=json -ast-dump-filter=RE`       | `ast.json` (namespaces `RE`, `REL`) |
//! | ast-F4SE  | `-fsyntax-only -ast-dump=json -ast-dump-filter=F4SE`     | `ast.json` (namespace `F4SE`) |
//! | vtables   | `-emit-llvm -g -fstandalone-debug -fdump-vtable-layouts` on a probe TU | `vtables.txt` |
//!
//! `--flavor ng` (the next-gen fork) adds an `ast-Scaleform` pass (the fork's top-level
//! `Scaleform` namespace) and a `shim-check` pass that compares our REL/REX shims'
//! Win32 value types with the Windows SDK's (`shim-ng/shim-ng-check.cpp`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Instant;

use serde_json::json;

use crate::ast::{self, Analysis, AstWriter, RelocKind, Walker};
use crate::clang::{self, Config, PassResult};
use crate::flavor::Flavor;
use crate::records::{self, RecordLayout};
use crate::thirdparty;

/// Stack for the AST threads: serde and the walker recurse once per JSON level.
const AST_STACK: usize = 512 << 20;

pub struct Options {
    pub clf4: PathBuf,
    pub clang: Option<PathBuf>,
    pub xwin: Option<PathBuf>,
    pub out: PathBuf,
    pub third_party: PathBuf,
    pub offline: bool,
    pub delayed_templates: bool,
    pub flavor: Flavor,
}

pub struct Clf4 {
    /// The checkout (contains `CommonLibF4/include`).
    pub root: PathBuf,
    pub include: PathBuf,
    pub commit: Option<String>,
    pub dirty: Option<bool>,
}

pub fn locate_clf4(path: &Path, flavor: Flavor) -> Result<Clf4, String> {
    let path = fs::canonicalize(path).map_err(|e| format!("CommonLibF4 at {}: {e}", path.display()))?;
    let (root, include) = if flavor == Flavor::Ng {
        // libxse/commonlibf4: <root>/include/{RE,F4SE,Scaleform}
        if !path.join("include/RE/Fallout.h").is_file() {
            return Err(format!("{} is not a next-gen CommonLibF4 checkout (no include/RE/Fallout.h)", path.display()));
        }
        (path.clone(), path.join("include"))
    } else if path.join("CommonLibF4/include/RE/Fallout.h").is_file() {
        (path.clone(), path.join("CommonLibF4/include"))
    } else if path.join("include/RE/Fallout.h").is_file() {
        (path.parent().unwrap_or(&path).to_path_buf(), path.join("include"))
    } else {
        return Err(format!(
            "{} is not a CommonLibF4 checkout (no CommonLibF4/include/RE/Fallout.h)",
            path.display()
        ));
    };
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let commit = git(&["rev-parse", "HEAD"]);
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty());
    Ok(Clf4 { root, include, commit, dirty })
}

struct RecordsSide {
    records_pass: PassResult,
    parse_seconds: f64,
    layouts: Vec<RecordLayout>,
    parse_warnings: Vec<String>,
    vtables_pass: PassResult,
    probes: usize,
    dropped_probes: Vec<String>,
}

struct AstSide {
    pass: PassResult,
    analysis: Analysis,
    dropped: Vec<String>,
}

/// Runs the dump. Returns `Ok(true)` when every pass finished without errors.
pub fn run(opts: &Options) -> Result<bool, String> {
    let start = Instant::now();
    let flavor = opts.flavor;
    let clf4_commit = flavor.commit();
    let umbrellas = flavor.umbrellas();
    let clf4 = locate_clf4(&opts.clf4, flavor)?;
    if clf4.commit.as_deref() != Some(clf4_commit) {
        eprintln!(
            "warning: CommonLibF4 is at {} but provenance/codegen.jsonl cites {clf4_commit}",
            clf4.commit.as_deref().unwrap_or("an unknown commit")
        );
    }
    let toolchain = clang::find_clang(opts.clang.as_deref())?;
    let xwin = clang::find_xwin(opts.xwin.as_deref())?;
    let third_party =
        if flavor.third_party() { thirdparty::ensure_all(&opts.third_party, opts.offline)? } else { Vec::new() };
    let shim = flavor.shim_dir();
    let cfg = Config {
        toolchain,
        xwin,
        third_party: third_party.clone(),
        clf4_include: clf4.include.clone(),
        delayed_templates: opts.delayed_templates,
        std: flavor.std(),
        defines: if flavor == Flavor::Og { clang::DEFINES } else { &[] },
        shim_includes: shim.iter().cloned().collect(),
    };

    fs::create_dir_all(&opts.out).map_err(|e| format!("creating {}: {e}", opts.out.display()))?;
    let out = fs::canonicalize(&opts.out).map_err(|e| format!("{}: {e}", opts.out.display()))?;
    let tu = out.join("tu.cpp");
    let tu_text: String = umbrellas.iter().map(|h| format!("#include \"{h}\"\n")).collect();
    fs::write(&tu, tu_text).map_err(|e| format!("writing {}: {e}", tu.display()))?;
    let clf4_prefix = format!("{}/", clf4.root.display());
    let ast_path = out.join("ast.json");

    let (records_side, ast_re, ast_f4se, ast_scaleform) = thread::scope(|s| {
        let records = s.spawn(|| records_and_vtables(&cfg, &out, &tu, umbrellas));
        let re = thread::Builder::new()
            .stack_size(AST_STACK)
            .spawn_scoped(s, || {
                let file = fs::File::create(&ast_path)
                    .map_err(|e| format!("creating {}: {e}", ast_path.display()))?;
                let mut writer =
                    AstWriter::new(BufWriter::with_capacity(1 << 20, file)).map_err(|e| e.to_string())?;
                let side =
                    ast_pass(&cfg, &out, &tu, "RE", &["RE", "REL"], &clf4_prefix, |v| writer.write_value(v))?;
                Ok::<_, String>((side, writer))
            })
            .expect("spawning the AST thread");
        let f4se = thread::Builder::new()
            .stack_size(AST_STACK)
            .spawn_scoped(s, || {
                let mut values = Vec::new();
                let side = ast_pass(&cfg, &out, &tu, "F4SE", &["F4SE"], &clf4_prefix, |v| {
                    values.push(v.to_vec());
                    Ok(())
                })?;
                Ok::<_, String>((side, values))
            })
            .expect("spawning the AST thread");
        // The next-gen fork moved Scaleform out of `RE` into its own top-level namespace.
        let scaleform = (flavor == Flavor::Ng).then(|| {
            thread::Builder::new()
                .stack_size(AST_STACK)
                .spawn_scoped(s, || {
                    let mut values = Vec::new();
                    let side = ast_pass(&cfg, &out, &tu, "Scaleform", &["Scaleform"], &clf4_prefix, |v| {
                        values.push(v.to_vec());
                        Ok(())
                    })?;
                    Ok::<_, String>((side, values))
                })
                .expect("spawning the AST thread")
        });
        (
            records.join().expect("records thread panicked"),
            re.join().expect("AST thread panicked"),
            f4se.join().expect("AST thread panicked"),
            scaleform.map(|t| t.join().expect("AST thread panicked")),
        )
    });
    let records_side = records_side?;
    let (ast_re, mut writer) = ast_re?;
    let (ast_f4se, f4se_values) = ast_f4se?;
    let ast_scaleform = ast_scaleform.transpose()?;
    for value in f4se_values.iter().chain(ast_scaleform.iter().flat_map(|(_, values)| values)) {
        writer.write_value(value).map_err(|e| format!("writing {}: {e}", ast_path.display()))?;
    }
    let ast_values = writer.finish().map_err(|e| format!("writing {}: {e}", ast_path.display()))?;
    drop(f4se_values);

    let mut analysis = Analysis::default();
    let mut dropped_top_level = ast_re.dropped.clone();
    dropped_top_level.extend(ast_f4se.dropped.iter().cloned());
    let mut passes = vec![records_side.records_pass.clone(), ast_re.pass.clone(), ast_f4se.pass.clone()];
    analysis.merge(ast_re.analysis);
    analysis.merge(ast_f4se.analysis);
    if let Some((side, _)) = ast_scaleform {
        dropped_top_level.extend(side.dropped.iter().cloned());
        passes.push(side.pass);
        analysis.merge(side.analysis);
    }
    passes.push(records_side.vtables_pass.clone());
    if let Some(shim) = &shim {
        passes.push(shim_check_pass(&cfg, &out, shim)?);
    }

    // --- cross-checks against the headers themselves
    let source = scan_source(&clf4)?;
    let layout_names: BTreeSet<&str> = records_side.layouts.iter().map(|r| r.name.as_str()).collect();
    let re_with_layout = analysis.re_records.iter().filter(|n| layout_names.contains(n.as_str())).count();
    let re_without_layout: Vec<&String> =
        analysis.re_records.iter().filter(|n| !layout_names.contains(n.as_str())).collect();

    let evaluated: BTreeSet<&str> =
        analysis.static_asserts.iter().filter(|(_, s)| s.evaluated).map(|(k, _)| k.as_str()).collect();
    let evaluated_sizeof = analysis.static_asserts.values().filter(|s| s.evaluated && s.sizeof).count();
    let evaluated_offsetof = analysis.static_asserts.values().filter(|s| s.evaluated && s.offsetof).count();
    let not_evaluated: Vec<&String> =
        source.static_asserts.keys().filter(|k| !evaluated.contains(k.as_str())).collect();
    let source_sizeof = source.static_asserts.values().filter(|s| **s).count();
    let sizeof_not_evaluated: Vec<&String> = source
        .static_asserts
        .iter()
        .filter(|(k, s)| **s && !evaluated.contains(k.as_str()))
        .map(|(k, _)| k)
        .collect();

    let mut relocs = analysis.relocs.clone();
    relocs.sort_by(|a, b| (&a.file, a.line, a.id).cmp(&(&b.file, b.line, b.id)));
    relocs.dedup_by(|a, b| {
        (a.id, &a.kind, &a.file, a.line, &a.target_type, &a.function)
            == (b.id, &b.kind, &b.file, b.line, &b.target_type, &b.function)
    });
    let relocations: Vec<&ast::Reloc> = relocs.iter().filter(|r| r.kind == RelocKind::Relocation).collect();
    let relocation_ids: BTreeSet<u64> = relocations.iter().map(|r| r.id).collect();
    let table_ids = relocs.iter().filter(|r| r.kind == RelocKind::Id).count();
    let ast_id_sites: BTreeSet<(String, u64)> = relocs.iter().map(|r| (r.file.clone(), r.id)).collect();
    let ids_missing: Vec<String> = source
        .id_sites
        .iter()
        .filter(|site| !ast_id_sites.contains(*site))
        .map(|(file, id)| format!("{file}: REL::ID({id})"))
        .collect();
    let relocs_path = out.join("relocs.json");
    {
        let file =
            fs::File::create(&relocs_path).map_err(|e| format!("creating {}: {e}", relocs_path.display()))?;
        let mut w = BufWriter::new(file);
        write_lines_array(&relocs, &mut w).map_err(|e| format!("writing {}: {e}", relocs_path.display()))?;
    }

    let vt_text = fs::read_to_string(out.join("vtables.txt")).unwrap_or_default();
    let vt = vtable_counts(&vt_text);
    let record_summary = records::summarize(&records_side.layouts);

    let size = |name: &str| fs::metadata(out.join(name)).map(|m| m.len()).unwrap_or(0);
    let outputs: BTreeMap<&str, u64> =
        ["ast.json", "records.txt", "vtables.txt", "layouts.json", "relocs.json"]
            .into_iter()
            .map(|n| (n, size(n)))
            .collect();
    let total_seconds = start.elapsed().as_secs_f64();
    let ok = passes.iter().all(PassResult::ok);

    let info = json!({
        "tool": "f4x-codegen dump",
        "ok": ok,
        "seconds": round2(total_seconds),
        "clang": cfg.toolchain,
        "target": clang::TARGET,
        "msvc_compatibility_version": clang::MSVC_VERSION,
        "delayed_template_parsing": cfg.delayed_templates,
        "flavor": flavor.name(),
        "std": cfg.std,
        "xwin": cfg.xwin,
        "commonlibf4": {
            "root": clf4.root,
            "commit": clf4.commit,
            "dirty": clf4.dirty,
            "provenance_commit": clf4_commit,
            "umbrellas": umbrellas,
        },
        "shims": shim,
        "third_party": thirdparty::DEPS.iter().zip(&third_party).map(|(d, inc)| json!({
            "name": d.name, "version": d.version, "license": d.license, "url": d.url,
            "sha256": d.sha256, "include": inc, "why": d.why,
        })).collect::<Vec<_>>(),
        "defines": cfg.defines.iter().map(|(d, why)| json!({ "name": d, "why": why })).collect::<Vec<_>>(),
        "passes": passes,
        "outputs": outputs,
        "records": {
            "summary": record_summary,
            "parse_seconds": round2(records_side.parse_seconds),
            "parse_warnings": records_side.parse_warnings,
            "ast_re_record_definitions": analysis.re_records.len(),
            "ast_re_records_with_layout": re_with_layout,
            "ast_re_records_without_layout": re_without_layout,
        },
        "vtables": {
            "probes": records_side.probes,
            "dropped_probes": records_side.dropped_probes,
            "vftables": vt.vftables,
            "vftable_index_tables": vt.index_tables,
            "most_derived_names": vt.most_derived,
        },
        "ast": {
            "values": ast_values,
            "top_level": analysis.top_level,
            "dropped_top_level": dropped_top_level,
            "nodes": analysis.nodes,
        },
        "static_asserts": {
            "source_sites": source.static_asserts.len(),
            "source_sizeof_sites": source_sizeof,
            "evaluated_sites": evaluated.len(),
            "evaluated_sizeof_sites": evaluated_sizeof,
            "evaluated_offsetof_sites": evaluated_offsetof,
            "not_evaluated": not_evaluated,
            "sizeof_not_evaluated": sizeof_not_evaluated,
        },
        "rel_ids": {
            "relocations": relocations.len(),
            "relocation_unique_ids": relocation_ids.len(),
            "table_ids": table_ids,
            "non_literal": analysis.non_literal_ids,
            "source_sites": source.id_sites.len(),
            "source_sites_missing_from_ast": ids_missing,
        },
    });
    let info_path = out.join("dump-info.json");
    fs::write(&info_path, serde_json::to_string_pretty(&info).expect("serializable") + "\n")
        .map_err(|e| format!("writing {}: {e}", info_path.display()))?;

    // --- human summary
    println!("f4x-codegen dump: {} -> {}", cfg.toolchain.version, out.display());
    println!(
        "  CommonLibF4 {} ({}), umbrellas {}",
        clf4.commit.as_deref().map_or("?", |c| &c[..c.len().min(12)]),
        match clf4.dirty {
            Some(false) => "clean",
            Some(true) => "DIRTY",
            None => "not a git checkout",
        },
        umbrellas.join(" + ")
    );
    for p in &passes {
        println!(
            "  pass {:<9} {:>6.1}s  {} errors, {} warnings, {:.1} MB stdout",
            p.name,
            p.seconds,
            p.errors,
            p.warnings,
            p.stdout_bytes as f64 / 1e6
        );
    }
    println!(
        "  records:  {} layouts ({} RE::, {} RE:: non-template, {} RE:: polymorphic)",
        record_summary.records,
        record_summary.re_records,
        record_summary.re_non_template,
        record_summary.re_polymorphic
    );
    println!(
        "  RE:: coverage: {re_with_layout} of {} non-template RE:: record definitions in the AST have a layout",
        analysis.re_records.len()
    );
    println!(
        "  vtables:  {} VFTables, {} index tables, {} probes ({} dropped)",
        vt.vftables,
        vt.index_tables,
        records_side.probes,
        records_side.dropped_probes.len()
    );
    println!(
        "  static_assert: {} of {} source sites evaluated ({} of {} sizeof); 0 errors means all of them held",
        evaluated.len(),
        source.static_asserts.len(),
        evaluated_sizeof,
        source_sizeof
    );
    println!(
        "  REL::ID: {} relocations ({} unique IDs), {} table IDs; {} source sites not in the AST",
        relocations.len(),
        relocation_ids.len(),
        table_ids,
        ids_missing.len()
    );
    println!(
        "  wrote ast.json {:.0} MB, records.txt {:.1} MB, vtables.txt {:.1} MB, layouts.json {:.1} MB, relocs.json, dump-info.json",
        outputs["ast.json"] as f64 / 1e6,
        outputs["records.txt"] as f64 / 1e6,
        outputs["vtables.txt"] as f64 / 1e6,
        outputs["layouts.json"] as f64 / 1e6
    );
    println!("  total {total_seconds:.1}s");
    if !ok {
        for p in passes.iter().filter(|p| !p.ok()) {
            eprintln!("error: pass {} failed; see {}", p.name, p.diagnostics.display());
        }
    }
    Ok(ok)
}

fn records_and_vtables(cfg: &Config, out: &Path, tu: &Path, umbrellas: &[&str]) -> Result<RecordsSide, String> {
    let records_txt = out.join("records.txt");
    let records_pass = clang::run_pass(
        cfg,
        "records",
        &[
            "-fsyntax-only",
            "-Xclang",
            "-fdump-record-layouts-complete",
            "-Xclang",
            "-fdump-record-layouts-canonical",
        ],
        tu,
        &out.join("diag-records.txt"),
        clang::to_file(&records_txt),
    )?;
    let t = Instant::now();
    let text =
        fs::read_to_string(&records_txt).map_err(|e| format!("reading {}: {e}", records_txt.display()))?;
    let parsed = records::parse(&text);
    drop(text);
    write_layouts(&parsed.records, &out.join("layouts.json"))?;
    let parse_seconds = t.elapsed().as_secs_f64();

    let probes = probe_names(&parsed.records);
    let (vtables_pass, dropped_probes) = vtables_pass(cfg, out, &probes, umbrellas)?;
    Ok(RecordsSide {
        records_pass,
        parse_seconds,
        layouts: parsed.records,
        parse_warnings: parsed.warnings,
        vtables_pass,
        probes: probes.len(),
        dropped_probes,
    })
}

pub fn write_layouts(records: &[RecordLayout], path: &Path) -> Result<(), String> {
    let file = fs::File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    let mut w = BufWriter::new(file);
    records::write_json(records, &mut w)
        .and_then(|()| w.flush())
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Polymorphic CommonLibF4 records whose printed name is also valid C++.
fn probe_names(records: &[RecordLayout]) -> Vec<String> {
    let names: BTreeSet<&str> = records
        .iter()
        .filter(|r| r.is_polymorphic())
        .map(|r| r.name.as_str())
        .filter(|n| ["RE::", "F4SE::", "REL::", "Scaleform::"].iter().any(|p| n.starts_with(p)))
        // Anonymous/lambda types and C's `_Bool` (how the dump spells `bool`) cannot be named.
        .filter(|n| !n.contains('(') && !n.contains("_Bool"))
        .collect();
    names.into_iter().map(str::to_string).collect()
}

/// Writes the probe TU. Returns its text and the line number of each probe.
fn probe_source(probes: &[String], umbrellas: &[&str]) -> (String, Vec<usize>) {
    let mut text = String::from(
        "// Generated by f4x-codegen. One global pointer per polymorphic CommonLibF4 record:\n\
         // with -g -fstandalone-debug clang emits full debug info for each type, and that\n\
         // computes (and with -fdump-vtable-layouts prints) its MS-ABI vftable layout.\n",
    );
    for h in umbrellas {
        text.push_str(&format!("#include \"{h}\"\n"));
    }
    // Inside namespace RE so that template arguments printed unqualified still resolve.
    text.push_str("namespace RE::f4x_probe {\n");
    let mut line = text.lines().count();
    let mut lines = Vec::with_capacity(probes.len());
    for (n, name) in probes.iter().enumerate() {
        line += 1;
        lines.push(line);
        text.push_str(&format!("::{name}* vt_{n} = nullptr;\n"));
    }
    text.push_str("}\n");
    (text, lines)
}

fn vtables_pass(
    cfg: &Config,
    out: &Path,
    probes: &[String],
    umbrellas: &[&str],
) -> Result<(PassResult, Vec<String>), String> {
    let probe_cpp = out.join("vtables-probe.cpp");
    let vtables_txt = out.join("vtables.txt");
    let diag = out.join("diag-vtables.txt");
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mode = [
        "-S",
        "-emit-llvm",
        "-o",
        null,
        "-g",
        "-gcodeview",
        "-fstandalone-debug",
        "-Xclang",
        "-fdump-vtable-layouts",
    ];
    let mut probes = probes.to_vec();
    let mut dropped = Vec::new();
    for attempt in 0..3 {
        let (text, lines) = probe_source(&probes, umbrellas);
        fs::write(&probe_cpp, text).map_err(|e| format!("writing {}: {e}", probe_cpp.display()))?;
        let pass = clang::run_pass(cfg, "vtables", &mode, &probe_cpp, &diag, clang::to_file(&vtables_txt))?;
        if pass.ok() || attempt == 2 {
            return Ok((pass, dropped));
        }
        // Drop probes whose name clang could not resolve and try again.
        let diag_text = fs::read_to_string(&diag).unwrap_or_default();
        let bad: BTreeSet<usize> = clang::error_lines_in(&diag_text, &probe_cpp).into_iter().collect();
        if bad.is_empty() {
            return Ok((pass, dropped));
        }
        let mut keep = Vec::new();
        for (name, line) in probes.into_iter().zip(lines) {
            if bad.contains(&line) { dropped.push(name) } else { keep.push(name) }
        }
        probes = keep;
    }
    unreachable!("the loop returns on its last attempt")
}

/// Next-gen only: compiles `shim-ng/shim-ng-check.cpp`, which includes the real
/// Windows SDK headers next to our shims and asserts that every Win32 value type the
/// engine records hold has the SDK's size and field offsets.
fn shim_check_pass(cfg: &Config, out: &Path, shim: &Path) -> Result<PassResult, String> {
    let check = shim.join("shim-ng-check.cpp");
    clang::run_pass(cfg, "shim-check", &["-fsyntax-only"], &check, &out.join("diag-shim-check.txt"), |mut stdout| {
        io::copy(&mut stdout, &mut io::sink()).map_err(|e| e.to_string())
    })
}

fn ast_pass(
    cfg: &Config,
    out: &Path,
    tu: &Path,
    filter: &str,
    keep: &[&str],
    clf4_prefix: &str,
    mut store: impl FnMut(&[u8]) -> io::Result<()>,
) -> Result<AstSide, String> {
    let mut analysis = Analysis::default();
    let mut dropped = Vec::new();
    let filter_arg = format!("-ast-dump-filter={filter}");
    let pass = clang::run_pass(
        cfg,
        &format!("ast-{filter}"),
        &["-fsyntax-only", "-Xclang", "-ast-dump=json", "-Xclang", &filter_arg],
        tu,
        &out.join(format!("diag-ast-{filter}.txt")),
        |stdout| {
            ast::split_stream(io::BufReader::with_capacity(1 << 20, stdout), |value| {
                let node = ast::parse_node(&value)?;
                // The filter is a substring match on qualified names, so it also picks
                // up strays like `_CRT_REPORT_HOOK`; keep whole namespaces only.
                let wanted =
                    node.kind == "NamespaceDecl" && node.name.as_deref().is_some_and(|n| keep.contains(&n));
                if wanted {
                    store(&value).map_err(|e| format!("writing ast.json: {e}"))?;
                    Walker::new(&mut analysis, clf4_prefix).top_level(&node);
                } else {
                    dropped.push(format!("{} {}", node.kind, node.name.as_deref().unwrap_or("")));
                }
                Ok(())
            })
        },
    )?;
    Ok(AstSide { pass, analysis, dropped })
}

struct SourceScan {
    /// `file:line` → whether the assertion mentions `sizeof`.
    static_asserts: BTreeMap<String, bool>,
    /// (file, id) for each `REL::ID(<literal>)` in the headers.
    id_sites: BTreeSet<(String, u64)>,
}

/// Textual scan of CommonLibF4's headers, to cross-check the AST.
fn scan_source(clf4: &Clf4) -> Result<SourceScan, String> {
    let mut files = Vec::new();
    collect_headers(&clf4.include, &mut files);
    files.sort();
    let mut scan = SourceScan { static_asserts: BTreeMap::new(), id_sites: BTreeSet::new() };
    for path in files {
        let rel = path.strip_prefix(&clf4.root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let text = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("static_assert(") {
                scan.static_asserts.insert(format!("{rel}:{}", n + 1), code.contains("sizeof("));
            }
            let mut rest = code;
            while let Some(at) = rest.find("REL::ID(") {
                rest = &rest[at + "REL::ID(".len()..];
                let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
                if let Ok(id) = digits.parse::<u64>() {
                    scan.id_sites.insert((rel.clone(), id));
                }
            }
        }
    }
    Ok(scan)
}

fn collect_headers(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_headers(&path, out);
        } else if path.extension().is_some_and(|e| e == "h" || e == "hpp" || e == "inl") {
            out.push(path);
        }
    }
}

struct VtableCounts {
    vftables: usize,
    index_tables: usize,
    most_derived: usize,
}

/// `VFTable for 'A' in 'B' in 'C' (N entries).` names C, the most-derived class.
fn vtable_counts(text: &str) -> VtableCounts {
    let mut most_derived = BTreeSet::new();
    let mut vftables = 0;
    let mut index_tables = 0;
    for line in text.lines() {
        if line.starts_with("VFTable indices for ") {
            index_tables += 1;
        } else if let Some(rest) = line.strip_prefix("VFTable for ") {
            vftables += 1;
            if let Some(name) = rest.split('\'').filter(|s| !s.is_empty()).step_by(2).last() {
                most_derived.insert(name.to_string());
            }
        }
    }
    VtableCounts { vftables, index_tables, most_derived: most_derived.len() }
}

fn write_lines_array<T: serde::Serialize>(items: &[T], w: &mut impl Write) -> io::Result<()> {
    w.write_all(b"[\n")?;
    for (n, item) in items.iter().enumerate() {
        serde_json::to_writer(&mut *w, item)?;
        w.write_all(if n + 1 == items.len() { b"\n" } else { b",\n" })?;
    }
    w.write_all(b"]\n")?;
    w.flush()
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_lines_match_text() {
        let probes = vec!["RE::A".to_string(), "RE::B<C>".to_string()];
        let (text, lines) = probe_source(&probes, &["RE/Fallout.h"]);
        let all: Vec<&str> = text.lines().collect();
        assert_eq!(all[lines[0] - 1], "::RE::A* vt_0 = nullptr;");
        assert_eq!(all[lines[1] - 1], "::RE::B<C>* vt_1 = nullptr;");
    }

    #[test]
    fn counts_vtables() {
        let text = "VFTable for 'RE::A' (3 entries).\n   0 | x\n\nVFTable indices for 'RE::A' (2 entries).\n\
                    VFTable for 'RE::X' in 'RE::Y' in 'RE::B' (1 entry).\n";
        let c = vtable_counts(text);
        assert_eq!((c.vftables, c.index_tables, c.most_derived), (2, 1, 2));
    }
}
