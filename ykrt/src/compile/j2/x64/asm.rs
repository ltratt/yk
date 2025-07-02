use crate::{compile::j2::x64::X64CompiledTrace, mt::TraceId};
use iced_x86::{Decoder, DecoderOptions, Encoder, Instruction as Inst};
use index_vec::{index_vec, IndexVec};
use libc::{mmap, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_EXEC, PROT_READ, PROT_WRITE};
use std::mem::replace;

pub(super) struct Asm {
    blocks: IndexVec<BlockIdx, IndexVec<InstIdx, Inst>>,
    insts: IndexVec<InstIdx, Inst>,
    /// `Br` and `Jmp` relocations. These are (and must!) always be sorted by the `InstIdx`.
    br_relocations: Vec<((BlockIdx, InstIdx), LabelIdx)>,
    br_labels: IndexVec<LabelIdx, Option<(BlockIdx, InstIdx)>>,
}

impl Asm {
    pub(super) fn new() -> Self {
        Asm {
            blocks: index_vec![],
            insts: index_vec![],
            br_relocations: vec![],
            br_labels: index_vec![],
        }
    }

    pub(super) fn block_completed(&mut self) {
        self.blocks.push(replace(&mut self.insts, index_vec![]));
    }

    pub(super) fn new_br_label(&mut self) -> LabelIdx {
        let lidx = self.br_labels.push(None);
        self.br_relocations
            .push(((self.blocks.len_idx(), self.insts.len_idx()), lidx));
        lidx
    }

    pub(super) fn set_br_label(&mut self, lidx: LabelIdx) {
        self.br_labels[lidx] = Some((self.blocks.len_idx(), self.insts.len_idx()));
    }

    pub(super) fn push_inst(
        &mut self,
        x64inst: Result<iced_x86::Instruction, iced_x86::IcedError>,
    ) {
        self.insts.push(x64inst.unwrap());
    }

    /// # Panics
    ///
    /// If `block_completed` has not been called immediately prior to this function.
    pub(super) fn into_exe(mut self, trid: TraceId) -> X64CompiledTrace {
        let buf = unsafe {
            mmap(
                std::ptr::null_mut(),
                page_size::get(),
                PROT_READ | PROT_WRITE | PROT_EXEC,
                MAP_ANON | MAP_PRIVATE,
                -1,
                0,
            )
        };
        if buf == MAP_FAILED {
            todo!();
        }

        // Convert the `Instruction`s into a byte sequence.
        let mut enc = Encoder::new(64);
        let base = u64::try_from(buf.addr()).unwrap();
        let mut off = 0;
        let mut offs = Vec::new();
        let mut b_offs = IndexVec::with_capacity(self.blocks.len());
        assert!(self.insts.is_empty());
        for b in self.blocks.iter_mut() {
            b_offs.push(offs.len());
            for inst in b.iter_mut().rev() {
                let addr = base + off;
                // At this point we don't necessarily know where jump instructions should go to. To
                // stop iced_x86 from complaining, we make all jumps go to an address -- that of
                // the instruction itself! which, by definition, is "near". We patch this below.
                if (inst.is_jmp_near() || inst.is_jcc_near()) && inst.near_branch32() == 0 {
                    inst.set_near_branch64(addr);
                }
                offs.push(off);
                let lenb = enc.encode(inst, addr).unwrap();
                off += u64::try_from(lenb).unwrap();
            }
        }

        // Fix relative jumps.
        let mut enc = enc.take_buffer();
        for ((bidx, iidx), lidx) in self.br_relocations.into_iter() {
            let jmp_iidx = usize::from(b_offs[bidx] + self.blocks[bidx].len() - iidx - 1);
            let jmp_off = usize::try_from(offs[jmp_iidx]).unwrap();

            // Check we really are about to patch a 32-bit RIP jump.
            let patch_off = if enc[jmp_off] == 0xe9 {
                jmp_off + 1 // JMP
            } else if enc[jmp_off..jmp_off + 2] == [0x0F, 0x83] {
                jmp_off + 2 // JAE
            } else {
                todo!()
            };

            let rip_iidx = usize::from(b_offs[bidx] + self.blocks[bidx].len() - iidx);

            let (to_bidx, to_iidx) = self.br_labels[lidx].unwrap();
            let to_iidx = usize::from(b_offs[to_bidx] + (self.blocks[to_bidx].len() - to_iidx));
            let off =
                i32::try_from(offs[to_iidx].checked_signed_diff(offs[rip_iidx]).unwrap()).unwrap();
            enc[patch_off..patch_off + 4].copy_from_slice(&off.to_le_bytes());
        }

        let mut dec = Decoder::with_ip(64, &enc, base, DecoderOptions::NONE);

        use iced_x86::Formatter;
        let mut fmtr = iced_x86::NasmFormatter::new();
        fmtr.options_mut().set_hex_prefix("0x");
        fmtr.options_mut().set_hex_suffix("");
        fmtr.options_mut().set_rip_relative_addresses(true);
        fmtr.options_mut().set_space_after_operand_separator(true);
        let mut inst = Inst::default();
        let mut out = String::new();
        for inst in &mut dec {
            out.push_str(&format!("{:x} ", inst.ip()));
            fmtr.format(&inst, &mut out);
            out.push('\n');
        }
        println!("{out}");

        unsafe {
            (buf as *mut u8).copy_from_nonoverlapping(enc.as_ptr(), enc.len());
        }
        X64CompiledTrace::new(trid, buf)
    }
}

index_vec::define_index_type! {
    pub(super) struct BlockIdx = u16;
}

index_vec::define_index_type! {
    pub(super) struct InstIdx = u32;
}

index_vec::define_index_type! {
    pub(in crate::compile::j2) struct LabelIdx = u32;
}
