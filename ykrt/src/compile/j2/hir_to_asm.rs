//! High-level HIR to asm.
//!
//! This module is where J2 does the high-level parts of HIR to asm conversion ([HirToAsm]). It has
//! no understanding of backend details: those are hidden behind the [HirToAsmBackend] trait.
//! Backends (e.g. x64) then implement that trait to do all the platform specific things they need
//! and want to.

use crate::{
    aotsmp::AOT_STACKMAPS,
    compile::{
        j2::{
            hir::*,
            regalloc::{RegAlloc, RegCnstr, RegExt, RegT, VarLoc, VarLocs},
        },
        CompilationError, CompiledTrace,
    },
    mt::TraceId,
};
use index_vec::{index_vec, IndexVec};
use smallvec::{smallvec, SmallVec};
use std::{assert_matches::assert_matches, sync::Arc};

pub(super) struct HirToAsm<'a, B: HirToAsmBackend> {
    m: &'a Mod,
    be: B,
}

impl<'a, B: HirToAsmBackend> HirToAsm<'a, B> {
    pub(super) fn new(m: &'a Mod, be: B) -> Self {
        Self { m, be }
    }

    pub(super) fn build(mut self) -> Result<Arc<dyn CompiledTrace>, CompilationError> {
        match &self.m.kind {
            ModKind::Loop {
                entry_safepoint_id,
                entry,
                body,
            } => {
                assert!(body.is_none());
                let (rec, _) = AOT_STACKMAPS
                    .as_ref()
                    .unwrap()
                    .get(usize::try_from(*entry_safepoint_id).unwrap());
                let vlocs = rec.live_vals.iter().map(B::smp_to_vloc).collect::<Vec<_>>();

                // Create the backwards jump at the end of a loop trace.
                let start_label = self.be.mk_label();
                self.be.loop_backwards_jump()?;

                // Assemble the body
                let glabels = self.p_block(entry, vlocs.clone(), vlocs, true)?;
                self.be.set_label(start_label);
                self.be.block_completed();

                // Assemble guards
                for label in glabels {
                    self.be.guard_exit();
                    self.be.set_label(label);
                    self.be.block_completed();
                }
                self.be.into_exe(self.m.trid)
            }
            ModKind::Guard { .. } => todo!(),
            ModKind::Coupler { .. } => todo!(),
        }
    }

    fn p_block(
        &mut self,
        b: &Block,
        entry_vlocs: Vec<VarLocs<B::Reg>>,
        exit_vlocs: Vec<VarLocs<B::Reg>>,
        guards_allowed: bool,
    ) -> Result<Vec<B::Label>, CompilationError> {
        let mut ra = RegAlloc::<B>::new(self.m, b);
        let mut inst_iter = b.insts_iter().rev().peekable();
        {
            let (_, Inst::Exit(Exit { iidxs })) = inst_iter.next().unwrap() else {
                panic!()
            };

            for (iidx, vlocs) in iidxs.iter().zip(exit_vlocs) {
                ra.set_exit_vloc(*iidx, vlocs);
            }
        }

        let mut glabels = Vec::new();
        loop {
            let Some((iidx, hinst)) = inst_iter.next() else {
                panic!()
            };
            match hinst {
                Inst::Add(x) => {
                    if ra.is_alive(iidx) {
                        self.be.i_add(&mut ra, b, iidx, x)?;
                    }
                }
                Inst::Call(x) => self.be.i_call(&mut ra, b, iidx, x)?,
                Inst::Const(_) => (),
                Inst::Exit(_) => unreachable!(),
                Inst::Guard(x) => {
                    assert!(guards_allowed);
                    glabels.push(self.be.mk_label());
                    self.be.i_guard(
                        &mut ra,
                        b,
                        iidx,
                        GuardIdx::new(b.guard_bodies.len() - glabels.len()),
                        x,
                    )?;
                }
                Inst::ICmp(x) => {
                    if ra.is_alive(iidx) {
                        self.be.i_icmp(&mut ra, b, iidx, x)?;
                    }
                }
                Inst::LoadArg(x) => unreachable!(),
                Inst::Load(x @ Load { is_volatile, .. }) => {
                    if *is_volatile || ra.is_alive(iidx) {
                        self.be.i_load(&mut ra, b, iidx, x)?;
                    }
                }
                Inst::Store(x) => self.be.i_store(&mut ra, b, iidx, x)?,
            }
            if let Some((_, Inst::LoadArg(_))) = inst_iter.peek() {
                break;
            }
        }

        for ((iidx, hinst), vlocs) in inst_iter.zip(entry_vlocs.into_iter().rev()) {
            assert_matches!(hinst, Inst::LoadArg(_));
            if ra.is_alive(iidx) {
                ra.set_entry_vloc(&mut self.be, iidx, vlocs)?;
            }
        }

        Ok(glabels)
    }
}

/// The trait that backends need to implement to assemble a trace into machine code.
pub(super) trait HirToAsmBackend {
    type Reg: RegT;
    type Label;

    fn smp_to_vloc(smp_locs: &SmallVec<[yksmp::Location; 1]>) -> VarLocs<Self::Reg>;

    /// Create a new label.
    fn mk_label(&mut self) -> Self::Label;
    /// Set `label` to the byte immediately after the end of the current instruction (or, looked at
    /// the other way around: to the first byte of the upcoming assembly instruction).
    fn set_label(&mut self, label: Self::Label);

    /// The current block has been completed.
    fn block_completed(&mut self);
    /// Assemble everything into machine code.
    fn into_exe(self, trid: TraceId) -> Result<Arc<dyn CompiledTrace>, CompilationError>;

    /// Produce code for the backwards jump that finishes a loop trace.
    fn loop_backwards_jump(&mut self) -> Result<(), CompilationError>;
    fn guard_exit(&mut self) -> Result<(), CompilationError>;

    /// Zero extend `c` to `tgt_bitw` bits. Note that `tgt_bitw` can equal to,
    /// or  greater than, `c`'s "natural" bit width.
    fn zero_ext_const(
        &mut self,
        reg: Self::Reg,
        tgt_bitw: u32,
        c: &ConstKind,
    ) -> Result<(), CompilationError>;

    /// Note: `stack_off` is not aligned in any way!
    ///
    /// Return how many bytes of stack were used.
    fn spill(
        &mut self,
        reg: Self::Reg,
        ext: RegExt,
        bitw: u32,
        stack_off: usize,
    ) -> Result<usize, CompilationError>;

    fn i_add(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        _: &Add,
    ) -> Result<(), CompilationError>;

    fn i_call(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        _: &Call,
    ) -> Result<(), CompilationError>;

    /// `gidx` will be in reverse order (i.e. from n..0).
    fn i_guard(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        gidx: GuardIdx,
        _: &Guard,
    ) -> Result<(), CompilationError>;

    fn i_icmp(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        _: &ICmp,
    ) -> Result<(), CompilationError>;

    fn i_load(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        _: &Load,
    ) -> Result<(), CompilationError>;

    fn i_store(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        _: &Store,
    ) -> Result<(), CompilationError>;
}
