//! A function index of one exe: functions from `.pdata` (chained unwind entries folded
//! into their owner, leaf functions added from call targets), each with the features the
//! matcher compares: calls in order, referenced strings, globals, vtables and constants.

use std::collections::{BTreeMap, HashMap, HashSet};

use f4x_pe::rtti::RttiIndex;
use f4x_pe::{Image, PeFile, Section};
use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, OpKind};

#[derive(Clone, Debug, Default)]
pub struct Func {
    pub start: u32,
    /// Code ranges (`[begin, end)`), the first one starting at `start`.
    pub chunks: Vec<(u32, u32)>,
    /// Direct call and tail-jump targets (function starts), in code order.
    pub calls: Vec<u32>,
    /// Strings referenced RIP-relatively, in code order.
    pub strings: Vec<String>,
    /// Writable globals (`.data`) referenced, in code order.
    pub data: Vec<u32>,
    /// Classes whose vtables are referenced, in code order.
    pub vtables: Vec<String>,
    /// Other read-only data referenced (`.rdata` constants, tables), in code order.
    pub consts: Vec<u32>,
    /// Distinctive immediates (>= 0x100, not addresses), in code order.
    pub imms: Vec<u64>,
    pub insns: u32,
    pub size: u32,
}

pub struct Binary<'a> {
    pub pe: PeFile<'a>,
    pub base: u64,
    pub text: Section,
    pub funcs: Vec<Func>,
    by_start: HashMap<u32, usize>,
    /// Sorted chunk ranges -> owning function index.
    chunks: Vec<(u32, u32, usize)>,
    /// vtable RVA -> demangled class name.
    pub vtables: HashMap<u32, String>,
    pub callers: Vec<Vec<usize>>,
}

const UNW_FLAG_CHAININFO: u8 = 0x4;

impl<'a> Binary<'a> {
    pub fn load(bytes: &'a [u8]) -> Result<Self, String> {
        let pe = PeFile::parse(bytes).map_err(|e| e.0)?;
        let base = pe.image_base();
        let text = pe.section(".text").ok_or("no .text")?.clone();
        let rtti = RttiIndex::scan(&pe);
        let vtables = rtti.vtables.iter().map(|v| (v.vtable_rva, f4x_pe::rtti::demangle(&v.class))).collect();

        // .pdata: (begin, end, unwind) entries; chained entries belong to the root they chain to.
        let entries = pdata(&pe);
        let by_begin: HashMap<u32, (u32, u32)> = entries.iter().map(|&(b, e, u)| (b, (e, u))).collect();
        let root_of = |mut begin: u32| -> u32 {
            for _ in 0..16 {
                let Some(&(_, unwind)) = by_begin.get(&begin) else { break };
                let Some(header) = pe.bytes(unwind, 4) else { break };
                if header[0] >> 3 & UNW_FLAG_CHAININFO == 0 {
                    break;
                }
                let codes = header[2] as u32;
                let chained = unwind + 4 + codes.div_ceil(2) * 4;
                match pe.u32_at(chained) {
                    Some(parent) if parent != begin => begin = parent,
                    _ => break,
                }
            }
            begin
        };
        let mut owners: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
        for &(begin, end, _) in &entries {
            if !text.contains(begin) {
                continue;
            }
            owners.entry(root_of(begin)).or_default().push((begin, end));
        }
        let mut binary = Binary { pe, base, text, funcs: Vec::new(), by_start: HashMap::new(), chunks: Vec::new(), vtables, callers: Vec::new() };
        for (start, mut chunks) in owners {
            chunks.sort_unstable();
            if let Some(pos) = chunks.iter().position(|c| c.0 == start) {
                chunks.swap(0, pos);
            }
            binary.add(Func { start, chunks, ..Func::default() });
        }
        // Decode; call targets outside every chunk are leaf functions (no unwind data).
        let mut pending: Vec<usize> = (0..binary.funcs.len()).collect();
        while let Some(index) = pending.pop() {
            let leaves = binary.decode(index);
            for leaf in leaves {
                if binary.owner(leaf).is_none() && binary.text.contains(leaf) {
                    let end = binary.leaf_end(leaf);
                    let new = binary.add(Func { start: leaf, chunks: vec![(leaf, end)], ..Func::default() });
                    pending.push(new);
                }
            }
        }
        // Normalise call targets to function starts (thunks followed) and build callers.
        let thunks: HashMap<u32, u32> = binary
            .funcs
            .iter()
            .filter(|f| f.insns == 1 && f.calls.len() == 1)
            .map(|f| (f.start, f.calls[0]))
            .collect();
        let starts: Vec<u32> = binary.funcs.iter().map(|f| f.start).collect();
        let mut callers = vec![Vec::new(); binary.funcs.len()];
        for (i, func) in binary.funcs.iter_mut().enumerate() {
            for call in &mut func.calls {
                if let Some(&target) = thunks.get(call) {
                    *call = target;
                }
            }
            for call in &func.calls {
                if let Some(&callee) = binary.by_start.get(call) {
                    callers[callee].push(i);
                }
            }
        }
        for list in &mut callers {
            list.sort_unstable();
            list.dedup();
        }
        binary.callers = callers;
        let _ = starts;
        Ok(binary)
    }

    fn add(&mut self, func: Func) -> usize {
        let index = self.funcs.len();
        self.by_start.insert(func.start, index);
        for &(b, e) in &func.chunks {
            let at = self.chunks.partition_point(|c| c.0 < b);
            self.chunks.insert(at, (b, e, index));
        }
        self.funcs.push(func);
        index
    }

    /// The function whose code contains `rva`.
    pub fn owner(&self, rva: u32) -> Option<usize> {
        let at = self.chunks.partition_point(|c| c.0 <= rva);
        let (b, e, owner) = *self.chunks.get(at.checked_sub(1)?)?;
        (b <= rva && rva < e).then_some(owner)
    }

    pub fn func_at(&self, start: u32) -> Option<&Func> {
        self.by_start.get(&start).map(|&i| &self.funcs[i])
    }

    pub fn index_of(&self, start: u32) -> Option<usize> {
        self.by_start.get(&start).copied()
    }

    pub fn code(&self, rva: u32, len: usize) -> &[u8] {
        let start = (rva - self.text.rva) as usize;
        let bytes = self.pe.section_bytes(&self.text);
        &bytes[start.min(bytes.len())..(start + len).min(bytes.len())]
    }

    fn leaf_end(&self, start: u32) -> u32 {
        let code = self.code(start, 4096);
        let mut decoder = Decoder::with_ip(64, code, self.base + start as u64, DecoderOptions::NONE);
        let mut end = start;
        let mut furthest = start as u64;
        for instruction in &mut decoder {
            if instruction.is_invalid() {
                break;
            }
            end = (instruction.next_ip() - self.base) as u32;
            if instruction.flow_control() == FlowControl::ConditionalBranch {
                furthest = furthest.max(instruction.near_branch_target() - self.base);
            }
            let stops = matches!(instruction.flow_control(), FlowControl::Return | FlowControl::UnconditionalBranch | FlowControl::IndirectBranch)
                || instruction.code() == iced_x86::Code::Int3;
            if stops && end as u64 > furthest {
                break;
            }
            if self.owner(end).is_some() {
                break;
            }
        }
        end
    }

    /// Decode function `index`, fill its features, and return call targets that are not
    /// known function starts.
    fn decode(&mut self, index: usize) -> Vec<u32> {
        let chunks = self.funcs[index].chunks.clone();
        let mut f = Func { start: self.funcs[index].start, chunks: chunks.clone(), ..Func::default() };
        let mut unknown = Vec::new();
        for &(b, e) in &chunks {
            f.size += e - b;
            let code = self.code(b, (e - b) as usize).to_vec();
            let mut decoder = Decoder::with_ip(64, &code, self.base + b as u64, DecoderOptions::NONE);
            let mut instruction = Instruction::default();
            while decoder.can_decode() {
                decoder.decode_out(&mut instruction);
                if instruction.is_invalid() {
                    continue;
                }
                f.insns += 1;
                match instruction.flow_control() {
                    FlowControl::Call | FlowControl::UnconditionalBranch if instruction.near_branch_target() != 0 => {
                        let target = (instruction.near_branch_target() - self.base) as u32;
                        let inside = chunks.iter().any(|&(cb, ce)| cb <= target && target < ce);
                        if !inside && self.text.contains(target) {
                            f.calls.push(target);
                            if !self.by_start.contains_key(&target) {
                                unknown.push(target);
                            }
                        }
                    }
                    _ => {}
                }
                if instruction.is_ip_rel_memory_operand() {
                    let target = (instruction.ip_rel_memory_address().wrapping_sub(self.base)) as u32;
                    self.classify(&mut f, target, &instruction);
                }
                for op in 0..instruction.op_count() {
                    let value = match instruction.op_kind(op) {
                        OpKind::Immediate32 | OpKind::Immediate32to64 | OpKind::Immediate64 => instruction.immediate(op),
                        OpKind::Immediate16 => instruction.immediate16() as u64,
                        _ => continue,
                    };
                    let v = value as i64;
                    if (0x100..0x1_0000_0000).contains(&v) && !(self.base as i64..self.base as i64 + 0x1000_0000).contains(&v) {
                        f.imms.push(value);
                    }
                }
            }
        }
        self.funcs[index] = f;
        unknown
    }

    fn classify(&self, f: &mut Func, target: u32, instruction: &Instruction) {
        let Some(section) = self.pe.section_at(target) else { return };
        if section.is_executable() {
            // A function pointer taken (lea rcx, [fn]): treat as a call-like edge.
            if instruction.mnemonic() == iced_x86::Mnemonic::Lea && self.text.contains(target) {
                f.calls.push(target);
            }
            return;
        }
        if section.is_writable() {
            f.data.push(target);
            return;
        }
        if let Some(class) = self.vtables.get(&target) {
            f.vtables.push(class.clone());
        } else if let Some(s) = self.string_at(target) {
            f.strings.push(s);
        } else {
            f.consts.push(target);
        }
    }

    /// A NUL-terminated ASCII (or UTF-16) string of at least 4 printable characters.
    pub fn string_at(&self, rva: u32) -> Option<String> {
        let bytes = self.pe.bytes(rva, 2)?;
        let wide = bytes.len() == 2 && bytes[1] == 0 && bytes[0] != 0;
        let mut out = String::new();
        let step = if wide { 2 } else { 1 };
        let mut at = rva;
        loop {
            let unit = self.pe.bytes(at, step)?;
            let c = if wide { u16::from_le_bytes([unit[0], unit[1]]) as u32 } else { unit[0] as u32 };
            if c == 0 {
                break;
            }
            let ch = char::from_u32(c)?;
            if !(ch.is_ascii_graphic() || ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r') {
                return None;
            }
            out.push(ch);
            at += step as u32;
            if out.len() > 512 {
                break;
            }
        }
        (out.len() >= 4).then(|| if wide { format!("L\"{out}\"") } else { out })
    }

    /// Every function that references `string` (exact match).
    pub fn functions_with_string(&self, string: &str) -> Vec<usize> {
        (0..self.funcs.len()).filter(|&i| self.funcs[i].strings.iter().any(|s| s == string)).collect()
    }

    /// RVAs of instructions in `.text` that reference `target` (RIP-relative or as a
    /// direct call/jump target).
    pub fn xrefs(&self, target: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for func in &self.funcs {
            for &(b, e) in &func.chunks {
                let code = self.code(b, (e - b) as usize);
                let mut decoder = Decoder::with_ip(64, code, self.base + b as u64, DecoderOptions::NONE);
                let mut instruction = Instruction::default();
                while decoder.can_decode() {
                    decoder.decode_out(&mut instruction);
                    let hit = (instruction.is_ip_rel_memory_operand() && instruction.ip_rel_memory_address() == self.base + target as u64)
                        || (instruction.near_branch_target() == self.base + target as u64
                            && matches!(instruction.flow_control(), FlowControl::Call | FlowControl::UnconditionalBranch));
                    if hit {
                        out.push((instruction.ip() - self.base) as u32);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Intel-syntax disassembly of `count` instructions at `rva`.
    pub fn disasm(&self, rva: u32, count: usize) -> Vec<String> {
        use iced_x86::Formatter as _;
        let code = self.code(rva, count * 15);
        let mut decoder = Decoder::with_ip(64, code, self.base + rva as u64, DecoderOptions::NONE);
        let mut formatter = iced_x86::IntelFormatter::new();
        let mut out = Vec::new();
        for instruction in decoder.iter().take(count) {
            let mut text = String::new();
            formatter.format(&instruction, &mut text);
            let mut note = String::new();
            if instruction.is_ip_rel_memory_operand() {
                let t = (instruction.ip_rel_memory_address().wrapping_sub(self.base)) as u32;
                if let Some(class) = self.vtables.get(&t) {
                    note = format!("  ; vtable {class}");
                } else if let Some(s) = self.string_at(t) {
                    note = format!("  ; {:?}", s);
                } else {
                    note = format!("  ; rva {t:#x}");
                }
            } else if instruction.near_branch_target() != 0 {
                note = format!("  ; rva {:#x}", instruction.near_branch_target() - self.base);
            }
            out.push(format!("{:#09x}  {text}{note}", instruction.ip() - self.base));
        }
        out
    }

    /// All strings referenced by any function, with the referencing functions.
    pub fn string_users(&self) -> HashMap<&str, Vec<usize>> {
        let mut map: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, f) in self.funcs.iter().enumerate() {
            let unique: HashSet<&str> = f.strings.iter().map(String::as_str).collect();
            for s in unique {
                map.entry(s).or_default().push(i);
            }
        }
        map
    }
}

fn pdata(pe: &PeFile) -> Vec<(u32, u32, u32)> {
    let Some(section) = pe.section(".pdata") else { return Vec::new() };
    let bytes = pe.section_bytes(section);
    bytes
        .as_chunks::<12>()
        .0
        .iter()
        .map(|e| {
            (
                u32::from_le_bytes([e[0], e[1], e[2], e[3]]),
                u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
                u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
            )
        })
        .filter(|&(b, e, _)| b != 0 && e > b)
        .collect()
}
