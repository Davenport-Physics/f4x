//! End-to-end CLI checks on a synthetic archive written under Cargo's target tmpdir.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use f4x_pex::*;

fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_f4x-pextool")).args(args).output().expect("run f4x-pextool")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A tiny script built through the public AST.
fn script(name: &str) -> Vec<u8> {
    let mut pex = Pex {
        header: Header {
            major_version: 3,
            minor_version: 9,
            game_id: FO4_GAME_ID,
            compilation_time: 0,
            source_file_name: format!("{name}.psc").as_str().into(),
            user_name: "".into(),
            machine_name: "".into(),
        },
        strings: StringTable::default(),
        debug_info: None,
        user_flags: Vec::new(),
        objects: Vec::new(),
    };
    let s = |pex: &mut Pex, t: &str| pex.strings.intern(t);
    let (object, parent, empty) = (s(&mut pex, name), s(&mut pex, "ScriptObject"), s(&mut pex, ""));
    let hidden = s(&mut pex, "hidden");
    pex.user_flags.push(UserFlag { name: hidden, bit: 0 });
    let native =
        pex.declare_function("Int", &[("Float", "afValue")], Function::FLAG_NATIVE | Function::FLAG_GLOBAL);
    let native_name = s(&mut pex, "Round");
    pex.objects.push(Object {
        name: object,
        parent_class_name: parent,
        doc_string: empty,
        is_const: false,
        user_flags: 1,
        auto_state_name: empty,
        structs: Vec::new(),
        variables: Vec::new(),
        properties: Vec::new(),
        states: vec![State {
            name: empty,
            functions: vec![NamedFunction { name: native_name, function: native }],
        }],
    });
    pex.to_bytes().unwrap()
}

/// Uncompressed GNRL archive.
fn archive(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut data_at = (24 + 36 * files.len()) as u64;
    let names_at = data_at + files.iter().map(|(_, d)| d.len() as u64).sum::<u64>();
    let mut out = b"BTDX".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(b"GNRL");
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    out.extend_from_slice(&names_at.to_le_bytes());
    for (name, data) in files {
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut ext = [0u8; 4];
        let e = name.rsplit('.').next().unwrap().as_bytes();
        ext[..e.len()].copy_from_slice(e);
        out.extend_from_slice(&ext);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0x0010_0100u32.to_le_bytes());
        out.extend_from_slice(&data_at.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&0xBAAD_F00Du32.to_le_bytes());
        data_at += data.len() as u64;
    }
    for (_, data) in files {
        out.extend_from_slice(data);
    }
    for (name, _) in files {
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    out
}

#[test]
fn list_extract_dump_and_roundtrip() {
    let good = tmp("cli-good.ba2");
    std::fs::write(
        &good,
        archive(&[
            ("Scripts\\Alpha.pex", script("Alpha")),
            ("scripts\\sub\\Beta.pex", script("Beta")),
            ("readme.txt", b"hi".to_vec()),
        ]),
    )
    .unwrap();
    let good = good.to_str().unwrap();

    let list = run(&["ba2", "list", good]);
    assert!(list.status.success());
    let text = stdout(&list);
    assert!(text.contains("BTDX v1 GNRL, 3 entries"), "{text}");
    assert!(text.contains("scripts\\sub\\Beta.pex"), "{text}");

    let outdir = tmp("cli-extract");
    let _ = std::fs::remove_dir_all(&outdir);
    let extract = run(&["ba2", "extract", good, "SCRIPTS/*.pex", outdir.to_str().unwrap()]);
    assert!(extract.status.success(), "{}", String::from_utf8_lossy(&extract.stderr));
    assert!(stdout(&extract).contains("extracted 2 file(s)"));
    let alpha = outdir.join("Scripts/Alpha.pex");
    assert_eq!(std::fs::read(&alpha).unwrap(), script("Alpha"));
    assert!(outdir.join("scripts/sub/Beta.pex").exists());
    assert!(!outdir.join("readme.txt").exists());

    let dump = run(&["pex", "dump", alpha.to_str().unwrap()]);
    assert!(dump.status.success());
    let text = stdout(&dump);
    assert!(text.contains("ScriptName Alpha extends ScriptObject hidden"), "{text}");
    assert!(text.contains("Int Function Round(Float afValue) Global Native"), "{text}");

    let from_archive = run(&["pex", "dump", "--ba2", good, "scripts/ALPHA.pex"]);
    assert_eq!(stdout(&from_archive), text);

    let rt = run(&["pex", "roundtrip", "--ba2", good]);
    assert!(rt.status.success());
    assert!(stdout(&rt).starts_with("2 .pex"), "{}", stdout(&rt));
    assert!(stdout(&rt).contains("2 identical, 0 failed"));
}

#[test]
fn roundtrip_reports_failures_and_exits_nonzero() {
    let mut broken = script("Broken");
    broken.push(0); // trailing byte: parse error
    let path = tmp("cli-bad.ba2");
    std::fs::write(&path, archive(&[("a.pex", script("Fine")), ("b.pex", broken)])).unwrap();

    let rt = run(&["pex", "roundtrip", "--ba2", path.to_str().unwrap()]);
    assert_eq!(rt.status.code(), Some(1));
    let text = stdout(&rt);
    assert!(text.contains("FAIL b.pex: parse failed at 0x"), "{text}");
    assert!(text.contains("1 identical, 1 failed (1 parse, 0 write, 0 mismatch)"), "{text}");
}

#[test]
fn bad_usage_exits_2() {
    assert_eq!(run(&["ba2"]).status.code(), Some(2));
    assert_eq!(run(&["nope", "x"]).status.code(), Some(2));
}
