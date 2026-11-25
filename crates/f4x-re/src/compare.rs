//! Evidence that an (old, new) pair is the same function or global in two builds: how
//! many of its strings, callees, callers, globals, immediates and vtables agree, read
//! through an existing function/global match. Used to check matcher candidates by hand.

use std::collections::{HashMap, HashSet};

use crate::index::Binary;

fn set<T: std::hash::Hash + Eq + Clone>(v: &[T]) -> HashSet<T> {
    v.iter().cloned().collect()
}

/// `k/m`: how many of the old side's mapped items the new side has.
fn mapped_overlap(old: &HashSet<u32>, new: &HashSet<u32>, map: &HashMap<u32, u32>) -> (usize, usize, usize) {
    let mapped: Vec<u32> = old.iter().filter_map(|o| map.get(o).copied()).collect();
    let found = mapped.iter().filter(|n| new.contains(n)).count();
    (found, mapped.len(), old.len())
}

#[allow(clippy::too_many_arguments)]
pub fn report(old: &Binary, new: &Binary, fmap: &HashMap<u32, u32>, gmap: &HashMap<u32, u32>, name: &str, kind: &str, o: u32, n: u32) -> String {
    let back: HashMap<u32, u32> = fmap.iter().map(|(&a, &b)| (b, a)).collect();
    let mut out = format!("{name} [{kind}] {o:#x} -> {n:#x}");
    if kind == "global" {
        let users = |b: &Binary, g: u32| -> HashSet<u32> { b.funcs.iter().filter(|f| f.data.contains(&g)).map(|f| f.start).collect() };
        let (ou, nu) = (users(old, o), users(new, n));
        let (k, m, t) = mapped_overlap(&ou, &nu, fmap);
        let (k2, m2, t2) = mapped_overlap(&nu, &ou, &back);
        let said = gmap.get(&o).map(|&g| if g == n { "agrees".to_owned() } else { format!("says {g:#x}") }).unwrap_or_else(|| "none".into());
        out += &format!("\n  users: old {t} (mapped {m}, of which {k} use new); new {t2} (mapped back {m2}, of which {k2} use old); match.json {said}");
        return out;
    }
    let (Some(oi), Some(ni)) = (old.index_of(o), new.index_of(n)) else {
        out += "\n  not a function start in one build";
        return out;
    };
    let (fo, fn_) = (&old.funcs[oi], &new.funcs[ni]);
    let (so, sn) = (set(&fo.strings), set(&fn_.strings));
    let common = so.intersection(&sn).count();
    let only_old: Vec<_> = so.difference(&sn).take(3).cloned().collect();
    let only_new: Vec<_> = sn.difference(&so).take(3).cloned().collect();
    let calls = mapped_overlap(&set(&fo.calls), &set(&fn_.calls), fmap);
    let calls_back = mapped_overlap(&set(&fn_.calls), &set(&fo.calls), &back);
    let oc: HashSet<u32> = old.callers[oi].iter().map(|&c| old.funcs[c].start).collect();
    let nc: HashSet<u32> = new.callers[ni].iter().map(|&c| new.funcs[c].start).collect();
    let callers = mapped_overlap(&oc, &nc, fmap);
    let data = mapped_overlap(&set(&fo.data), &set(&fn_.data), gmap);
    let (io, in_) = (set(&fo.imms), set(&fn_.imms));
    let (vo, vn) = (set(&fo.vtables), set(&fn_.vtables));
    let said = fmap.get(&o).map(|&g| if g == n { "agrees".to_owned() } else { format!("says {g:#x}") }).unwrap_or_else(|| "none".into());
    let taken = back.get(&n).filter(|&&x| x != o).map(|x| format!(" (new is matched to old {x:#x}!)")).unwrap_or_default();
    out += &format!(
        "\n  size {:#x}/{:#x} insns {}/{}; strings {common}/{}+{}; calls {}/{} of {} (back {}/{} of {}); callers {}/{} of {} (new has {}); data {}/{} of {}; imms {}/{}; vtables {}/{}; match.json {said}{taken}",
        fo.size, fn_.size, fo.insns, fn_.insns, so.len(), sn.len(),
        calls.0, calls.1, calls.2, calls_back.0, calls_back.1, calls_back.2,
        callers.0, callers.1, callers.2, nc.len(), data.0, data.1, data.2,
        io.intersection(&in_).count(), io.union(&in_).count(), vo.intersection(&vn).count(), vo.union(&vn).count(),
    );
    if !only_old.is_empty() || !only_new.is_empty() {
        out += &format!("\n  strings old-only {only_old:?} new-only {only_new:?}");
    }
    out
}
