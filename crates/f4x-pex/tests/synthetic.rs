//! A hand-assembled script covering every section and FO4 addition. The bytes are built
//! here field by field (not with the crate's writer), so these tests pin the layout.

use f4x_pex::*;

const STRINGS: &[&str] = &[
    "Synthetic",       // 0 object
    "",                // 1
    "ScriptObject",    // 2
    "hidden",          // 3
    "conditional",     // 4
    "Point",           // 5 struct
    "X",               // 6
    "Int",             // 7
    "doc for X",       // 8
    "Count",           // 9 auto property
    "::Count_var",     // 10
    "Ready",           // 11 state
    "GetValue",        // 12
    "aiBase",          // 13
    "None",            // 14
    "OnInit",          // 15
    "::temp0",         // 16
    "Float",           // 17
    "Bool",            // 18
    "Total",           // 19 get/set property
    "value",           // 20
    "Group1",          // 21
    "Synthetic#Point", // 22
    "hello",           // 23
    "self",            // 24
    "Debug",           // 25
    "Trace",           // 26
    "::nonevar",       // 27
    "::pt",            // 28
];

#[derive(Default)]
struct B {
    out: Vec<u8>,
}

impl B {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.out.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.out.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.out.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.out.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn ws(&mut self, s: &str) -> &mut Self {
        self.u16(s.len() as u16);
        self.out.extend_from_slice(s.as_bytes());
        self
    }
    fn ident(&mut self, id: u16) -> &mut Self {
        self.u8(1).u16(id)
    }
    fn string(&mut self, id: u16) -> &mut Self {
        self.u8(2).u16(id)
    }
    fn int(&mut self, v: i32) -> &mut Self {
        self.u8(3).u32(v as u32)
    }
    fn float(&mut self, v: f32) -> &mut Self {
        self.u8(4).u32(v.to_bits())
    }
    fn boolean(&mut self, v: bool) -> &mut Self {
        self.u8(5).u8(v as u8)
    }
    fn none(&mut self) -> &mut Self {
        self.u8(0)
    }
    /// return type, doc, user flags, flags, params, locals, then `instructions` count.
    fn function_head(
        &mut self,
        ret: u16,
        flags: u8,
        params: &[(u16, u16)],
        locals: &[(u16, u16)],
        n: u16,
    ) -> &mut Self {
        self.u16(ret).u16(1).u32(0).u8(flags);
        self.u16(params.len() as u16);
        for &(name, ty) in params {
            self.u16(name).u16(ty);
        }
        self.u16(locals.len() as u16);
        for &(name, ty) in locals {
            self.u16(name).u16(ty);
        }
        self.u16(n)
    }
}

/// Byte offsets of fields the corruption tests patch.
struct Offsets {
    object_size: usize,
    parent_index: usize,
    object_const: usize,
    first_opcode: usize,
    vararg_count_tag: usize,
}

fn fixture() -> (Vec<u8>, Offsets) {
    let mut b = B::default();
    // Header.
    b.u32(0xFA57_C0DE).u8(3).u8(9).u16(2).u64(0x5DDC_0CBF);
    b.ws("c:\\src\\Synthetic.psc").ws("builder").ws("MACHINE");
    // String table.
    b.u16(STRINGS.len() as u16);
    for s in STRINGS {
        b.ws(s);
    }
    // Debug info: functions, property groups, struct orders.
    b.u8(1).u64(0x5525_51F8);
    b.u16(2);
    b.u16(0).u16(1).u16(15).u8(0).u16(9);
    for line in 10..19 {
        b.u16(line);
    }
    b.u16(0).u16(9).u16(19).u8(1).u16(1).u16(30);
    b.u16(1).u16(0).u16(21).u16(1).u32(0x8).u16(2).u16(9).u16(19);
    b.u16(1).u16(0).u16(5).u16(1).u16(6);
    // User flags.
    b.u16(2).u16(3).u8(0).u16(4).u8(1);
    // Object.
    b.u16(1).u16(0);
    let object_size = b.out.len();
    b.u32(0);
    let parent_index = b.out.len();
    b.u16(2).u16(1);
    let object_const = b.out.len();
    b.u8(0).u32(0b10).u16(11);
    // Structs: Point { Int X = 5 ; doc }.
    b.u16(1).u16(5).u16(1).u16(6).u16(7).u32(0).int(5).u8(0).u16(8);
    // Variables.
    b.u16(1).u16(10).u16(7).u32(0).int(3).u8(1);
    // Properties: Count (auto), Total (get + set).
    b.u16(2);
    b.u16(9).u16(7).u16(1).u32(0b1).u8(7).u16(10);
    b.u16(19).u16(7).u16(1).u32(0).u8(3);
    b.function_head(7, 0, &[], &[], 1).u8(0x1A).ident(10);
    b.function_head(14, 0, &[(20, 7)], &[], 1).u8(0x0D).ident(10).ident(20);
    // States: default with a native and OnInit; "Ready" empty.
    b.u16(2);
    b.u16(1).u16(2);
    b.u16(12).function_head(7, 3, &[(13, 7)], &[], 0);
    b.u16(15).function_head(14, 0, &[], &[(16, 17), (27, 14), (28, 22)], 9);
    let first_opcode = b.out.len();
    b.u8(0x25).ident(28); // struct_create ::pt
    b.u8(0x27).ident(28).ident(6).int(-7); // struct_set ::pt, X, -7
    b.u8(0x26).ident(16).ident(28).ident(6); // struct_get ::temp0, ::pt, X
    b.u8(0x24).ident(16).ident(24).ident(7); // is ::temp0, self, Int
    b.u8(0x17).ident(12).ident(24).ident(16);
    let vararg_count_tag = b.out.len();
    b.int(5).int(1).float(2.5).string(23).boolean(true).none(); // callmethod GetValue, self, ::temp0, [..5]
    b.u8(0x19).ident(25).ident(26).ident(27).int(1).string(23); // callstatic Debug, Trace, ::nonevar, ["hello"]
    b.u8(0x16).ident(16).int(2); // jmpf ::temp0, +2
    b.u8(0x28).ident(28).ident(16).string(6).int(1).int(0); // array_findstruct
    b.u8(0x1A).none(); // return None
    b.u16(11).u16(0);
    let size = (b.out.len() - object_size) as u32;
    b.out[object_size..object_size + 4].copy_from_slice(&size.to_le_bytes());
    (b.out, Offsets { object_size, parent_index, object_const, first_opcode, vararg_count_tag })
}

#[test]
fn parses_every_section() {
    let (bytes, _) = fixture();
    let pex = Pex::parse(&bytes).unwrap();
    assert_eq!(pex.header.game_id, 2);
    assert_eq!(pex.header.source_file_name.to_str(), "c:\\src\\Synthetic.psc");
    assert_eq!(pex.strings.len(), STRINGS.len());

    let debug = pex.debug_info.as_ref().unwrap();
    assert_eq!(debug.functions.len(), 2);
    assert_eq!(debug.functions[0].line_numbers, (10..19).collect::<Vec<u16>>());
    assert_eq!(debug.functions[1].function_type, DebugFunctionType::Getter);
    assert_eq!(pex.str(debug.property_groups[0].group_name), "Group1");
    assert_eq!(debug.property_groups[0].user_flags, 0x8);
    assert_eq!(debug.property_groups[0].properties, [StringId(9), StringId(19)]);
    assert_eq!(debug.struct_orders[0].members, [StringId(6)]);

    assert_eq!(pex.user_flag_names(0b11), ["hidden", "conditional"]);
    assert_eq!(pex.object_names(), ["Synthetic"]);

    let object = pex.object("SYNTHETIC").unwrap();
    assert_eq!(pex.str(object.parent_class_name), "ScriptObject");
    assert!(!object.is_const);
    assert_eq!(pex.str(object.auto_state_name), "Ready");
    let member = &object.structs[0].members[0];
    assert_eq!(
        (pex.str(member.name), member.default, pex.str(member.doc_string)),
        ("X".into(), Value::Integer(5), "doc for X".into())
    );
    assert!(object.variables[0].is_const);

    let count = &object.properties[0];
    assert!(count.is_auto());
    assert_eq!(count.auto_var_name, Some(StringId(10)));
    assert!(count.read_handler.is_none() && count.write_handler.is_none());
    let total = &object.properties[1];
    assert!(total.auto_var_name.is_none());
    assert_eq!(total.read_handler.as_ref().unwrap().instructions[0].op, Opcode::Return);
    assert_eq!(total.write_handler.as_ref().unwrap().params.len(), 1);

    let on_init = &object.states[0].functions[1].function;
    let ops: Vec<_> = on_init.instructions.iter().map(|i| i.op.mnemonic()).collect();
    assert_eq!(
        ops,
        [
            "struct_create",
            "struct_set",
            "struct_get",
            "is",
            "callmethod",
            "callstatic",
            "jmpf",
            "array_findstruct",
            "return"
        ]
    );
    let call = &on_init.instructions[4];
    assert_eq!(call.args.len(), 3);
    assert_eq!(
        call.varargs,
        [Value::Integer(1), Value::Float(2.5), Value::String(StringId(23)), Value::Bool(true), Value::None]
    );
    assert_eq!(on_init.instructions[1].args[2], Value::Integer(-7));
    assert!(object.states[1].functions.is_empty());
}

#[test]
fn writes_identical_bytes() {
    let (bytes, _) = fixture();
    assert_eq!(Pex::parse(&bytes).unwrap().to_bytes().unwrap(), bytes);
}

#[test]
fn signatures_and_function_listing() {
    let (bytes, _) = fixture();
    let pex = Pex::parse(&bytes).unwrap();
    let natives: Vec<String> = pex.native_functions().iter().map(ToString::to_string).collect();
    assert_eq!(natives, ["Int Function GetValue(Int aiBase) Global Native"]);

    let all = pex.function_signatures();
    let listed: Vec<(FunctionKind, String, String)> =
        all.iter().map(|s| (s.kind, s.owner.clone(), s.to_string())).collect();
    assert_eq!(
        listed,
        [
            (FunctionKind::State, String::new(), "Int Function GetValue(Int aiBase) Global Native".into()),
            (FunctionKind::State, String::new(), "Function OnInit()".into()),
            (FunctionKind::Getter, "Total".into(), "Int Function Get()".into()),
            (FunctionKind::Setter, "Total".into(), "Function Set(Int value)".into()),
        ]
    );
    assert!(all[1].return_type.is_none());
}

#[test]
fn edits_survive_a_round_trip() {
    let (bytes, _) = fixture();
    let mut pex = Pex::parse(&bytes).unwrap();
    let before = pex.strings.len();

    // A new native in the default state, an empty event, a struct and a second object.
    let native = pex.declare_function(
        "Float",
        &[("Int", "aiBase"), ("objectreference", "akRef")],
        Function::FLAG_NATIVE,
    );
    let event = pex.declare_function("None", &[], 0);
    let (sqrt, on_load, extra, y) = (
        pex.strings.intern("Sqrt"),
        pex.strings.intern("OnLoad"),
        pex.strings.intern("ExtraScript"),
        pex.strings.intern("Y"),
    );
    assert_eq!(pex.strings.intern_ignore_case("INT"), StringId(7));
    let int = StringId(7);
    let empty = StringId(1);
    let object = &mut pex.objects[0];
    object.states[0].functions.push(NamedFunction { name: sqrt, function: native });
    object.states[0].functions.push(NamedFunction { name: on_load, function: event });
    object.structs.push(Struct {
        name: y,
        members: vec![StructMember {
            name: y,
            type_name: int,
            user_flags: 0,
            default: Value::Float(f32::from_bits(0x7FC0_1234)),
            is_const: false,
            doc_string: empty,
        }],
    });
    let mut second = pex.objects[0].clone();
    second.name = extra;
    pex.objects.push(second);

    // New: "objectreference", "akRef", "Sqrt", "OnLoad", "ExtraScript", "Y".
    // Reused: "Float", "Int", "aiBase", "None", "".
    assert_eq!(pex.strings.len(), before + 6);
    let out = pex.to_bytes().unwrap();
    let reparsed = Pex::parse(&out).unwrap();
    assert_eq!(reparsed.to_bytes().unwrap(), out);
    assert_eq!(reparsed.objects.len(), 2);
    assert!(
        reparsed
            .native_functions()
            .iter()
            .any(|s| s.to_string() == "Float Function Sqrt(Int aiBase, objectreference akRef) Native")
    );
    let Value::Float(nan) = reparsed.objects[0].structs[1].members[0].default else { panic!() };
    assert_eq!(nan.to_bits(), 0x7FC0_1234, "float bits are preserved");
}

#[test]
fn corrupt_inputs_are_errors() {
    let (bytes, at) = fixture();
    let parse = |patch: &dyn Fn(&mut Vec<u8>)| {
        let mut b = bytes.clone();
        patch(&mut b);
        Pex::parse(&b).unwrap_err()
    };

    assert!(matches!(parse(&|b| b[0] = 0).kind, ParseErrorKind::BadMagic(_)));
    let big_endian = parse(&|b| b[..4].copy_from_slice(&0xFA57_C0DEu32.to_be_bytes()));
    assert_eq!(big_endian.kind, ParseErrorKind::BadMagic(0xDEC0_57FA));
    assert_eq!(parse(&|b| b[5] = 2).kind, ParseErrorKind::UnsupportedVersion { major: 3, minor: 2 });
    assert_eq!(parse(&|b| b.push(0)).kind, ParseErrorKind::TrailingBytes(1));

    let e = parse(&|b| b[at.parent_index..at.parent_index + 2].copy_from_slice(&999u16.to_le_bytes()));
    assert_eq!(e.kind, ParseErrorKind::StringIndexOutOfRange { index: 999, count: STRINGS.len() });
    assert_eq!(e.offset, at.parent_index);

    let e = parse(&|b| b[at.object_size] ^= 1);
    assert!(matches!(e.kind, ParseErrorKind::ObjectSizeMismatch { .. }));
    assert_eq!(e.offset, at.object_size);

    assert_eq!(parse(&|b| b[at.object_const] = 2).kind, ParseErrorKind::BadBool(2));
    let e = parse(&|b| b[at.first_opcode] = 0x2F);
    assert_eq!((e.kind, e.offset), (ParseErrorKind::UnknownOpcode(0x2F), at.first_opcode));
    assert_eq!(parse(&|b| b[at.vararg_count_tag] = 2).kind, ParseErrorKind::BadVarargCount(2));
    assert_eq!(parse(&|b| b[at.vararg_count_tag] = 9).kind, ParseErrorKind::BadValueTag(9));
}

#[test]
fn truncation_and_bit_flips_never_panic() {
    let (bytes, _) = fixture();
    for len in 0..bytes.len() {
        let e = Pex::parse(&bytes[..len]).unwrap_err();
        assert!(e.offset <= len, "error offset {} beyond input {len}", e.offset);
    }
    for i in 0..bytes.len() {
        let mut b = bytes.clone();
        b[i] ^= 0xFF;
        if let Ok(pex) = Pex::parse(&b) {
            // Anything that still parses must still round-trip.
            assert_eq!(pex.to_bytes().unwrap(), b, "flip at {i}");
        }
    }
}

#[test]
fn inconsistent_trees_are_write_errors() {
    let (bytes, _) = fixture();
    let pex = Pex::parse(&bytes).unwrap();

    let mut bad = pex.clone();
    bad.objects[0].properties[0].auto_var_name = None;
    assert!(matches!(bad.to_bytes(), Err(WriteError::PropertyFlags { property: 9, flags: 7 })));

    let mut bad = pex.clone();
    bad.objects[0].states[0].functions[1].function.instructions[0].args.clear();
    assert!(matches!(bad.to_bytes(), Err(WriteError::Operands { op: "struct_create", .. })));

    let mut bad = pex.clone();
    bad.objects[0].doc_string = StringId(500);
    assert!(matches!(bad.to_bytes(), Err(WriteError::StringIndexOutOfRange { index: 500, .. })));

    let mut bad = pex;
    bad.objects[0].variables = vec![bad.objects[0].variables[0].clone(); 70_000];
    assert!(matches!(bad.to_bytes(), Err(WriteError::TooMany { what: "variables", len: 70_000 })));
}

#[test]
fn no_debug_info() {
    let (bytes, _) = fixture();
    let mut pex = Pex::parse(&bytes).unwrap();
    pex.debug_info = None;
    let out = pex.to_bytes().unwrap();
    let reparsed = Pex::parse(&out).unwrap();
    assert!(reparsed.debug_info.is_none());
    assert_eq!(reparsed.objects, pex.objects);
}
