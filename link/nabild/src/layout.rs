//! Output sections and segments: atoms placed in order at their
//! alignment, sections packed into page-aligned segments.

use crate::symtab::{Atom, AtomId};

#[derive(Debug, Default)]
pub struct OutSection {
    pub seg: usize,
    pub name: String,
    /// Format specific type and attributes.
    pub flags: u32,
    /// log2.
    pub align: u8,
    pub atoms: Vec<AtomId>,
    pub size: u64,
    pub addr: u64,
    pub offset: u64,
    /// Occupies memory only.
    pub zerofill: bool,
}

#[derive(Debug, Default)]
pub struct Segment {
    pub name: String,
    /// Initial and maximum protection.
    pub prot: u32,
    pub sects: Vec<usize>,
    pub addr: u64,
    pub vmsize: u64,
    pub offset: u64,
    pub filesize: u64,
}

/// Size `s` from its atoms (synthetic sections keep their size) and
/// give each atom its offset in `s` (as `addr`, rebased by [`assign`]).
pub fn place_atoms(s: &mut OutSection, atoms: &mut [Atom]) {
    let mut at = s.size;
    for &i in &s.atoms {
        let a = &mut atoms[i as usize];
        at = at.next_multiple_of(1 << a.align);
        a.addr = at;
        at += a.size;
        s.align = s.align.max(a.align);
    }
    s.size = at;
}

/// Addresses and file offsets: segments in order from `base`, the first
/// after `header` bytes of its own; atoms move with their sections.
pub fn assign(
    segs: &mut [Segment],
    sects: &mut [OutSection],
    atoms: &mut [Atom],
    base: u64,
    header: u64,
    page: u64,
) {
    let (mut addr, mut off) = (base, 0u64);
    for (k, seg) in segs.iter_mut().enumerate() {
        seg.addr = addr;
        seg.offset = off;
        let mut at = if k == 0 { header } else { 0 };
        let mut file_end = at;
        for &si in &seg.sects {
            let s = &mut sects[si];
            at = at.next_multiple_of(1 << s.align);
            s.addr = seg.addr + at;
            s.offset = if s.zerofill { 0 } else { seg.offset + at };
            at += s.size;
            if !s.zerofill {
                file_end = at;
            }
            for &i in &s.atoms {
                atoms[i as usize].addr += s.addr;
            }
        }
        seg.vmsize = at.next_multiple_of(page);
        seg.filesize = file_end.next_multiple_of(page);
        addr += seg.vmsize;
        off += seg.filesize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_page_aligned_and_zerofill_takes_no_file() {
        let mut atoms = vec![Atom::new(0, 0, 0, 10, 2), Atom::new(0, 0, 0, 3, 4)];
        let mut sects = vec![
            OutSection {
                atoms: vec![0, 1],
                ..OutSection::default()
            },
            OutSection {
                seg: 1,
                size: 100,
                zerofill: true,
                ..OutSection::default()
            },
        ];
        let (a, b) = sects.split_at_mut(1);
        place_atoms(&mut a[0], &mut atoms);
        place_atoms(&mut b[0], &mut atoms);
        assert_eq!(sects[0].size, 19);
        let mut segs = vec![
            Segment {
                sects: vec![0],
                ..Segment::default()
            },
            Segment {
                sects: vec![1],
                ..Segment::default()
            },
        ];
        assign(&mut segs, &mut sects, &mut atoms, 0x1000_0000, 40, 0x4000);
        assert_eq!(sects[0].addr, 0x1000_0030);
        assert_eq!(atoms[1].addr, 0x1000_0030 + 16);
        assert_eq!(segs[1].addr, 0x1000_4000);
        assert_eq!(segs[1].filesize, 0);
        assert_eq!(segs[1].vmsize, 0x4000);
    }
}
