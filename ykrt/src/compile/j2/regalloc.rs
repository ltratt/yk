//! The architecture independent part of register allocation.
//!
//! This register allocator fundamentally assumes it is involved in backwards code generation. A
//! forward register allocator ensures that, at the point of each allocation, the system is in the
//! correct state for the current instruction. In contrast, a backwards register allocator ensures
//! that, at the point of each allocation, the system is in the correct state for the previous
//! instruction (where "previous" is used "in the sense of backwards iteration").

use crate::compile::{
    j2::{
        hir::{Block, Const, ConstKind, Inst, InstIdx, Mod},
        hir_to_asm::HirToAsmBackend,
    },
    CompilationError,
};
use index_vec::{index_vec, Idx, IndexVec};
use smallvec::{smallvec, SmallVec};
use std::fmt;

pub(super) struct RegAlloc<'a, B: HirToAsmBackend + ?Sized> {
    m: &'a Mod,
    b: &'a Block,
    /// The state of each instruction.
    istate: IndexVec<InstIdx, SmallVec<[VarLoc<B::Reg>; 2]>>,
    /// The state of each register.
    rstate:
        IndexVec<<<B as HirToAsmBackend>::Reg as RegT>::RegIdx, (RegExt, SmallVec<[InstIdx; 2]>)>,
    /// What is the last point a given instruction is used at? The initial value of zero implicitly
    /// means "not used at all". This is safe, even for the instruction at position zero, because a
    /// value, by definition, cannot use itself.
    alive_until: IndexVec<InstIdx, InstIdx>,
    stack_off: usize,
    /// Constants which need to be put in registers at the end of the next (or, depending on how
    /// you look at it, the beginning of the current) instruction. This *must* be `drain`ed after
    /// each instruction.
    next_consts: SmallVec<[(B::Reg, RegExt, u32, &'a ConstKind); 4]>,
}

impl<'a, B: HirToAsmBackend> RegAlloc<'a, B> {
    pub(super) fn new(m: &'a Mod, b: &'a Block) -> Self {
        Self {
            m,
            b,
            istate: index_vec![SmallVec::new(); b.insts.len()],
            rstate: index_vec![(RegExt::Undefined, SmallVec::new()); B::Reg::max_regidx().index()],
            alive_until: index_vec![InstIdx::from_raw(0); b.insts.len()],
            stack_off: 0,
            next_consts: smallvec![],
        }
    }

    pub(super) fn set_entry_vloc(
        &mut self,
        be: &mut B,
        iidx: InstIdx,
        vlocs: VarLocs<B::Reg>,
    ) -> Result<(), CompilationError> {
        self.inst_entry(be, iidx)?;
        for vloc in vlocs.iter() {
            if !self.istate[iidx].contains(vloc) {
                todo!();
            }
        }
        Ok(())
    }

    pub(super) fn set_exit_vloc(&mut self, iidx: InstIdx, vlocs: VarLocs<B::Reg>) {
        for vloc in vlocs.iter() {
            if let VarLoc::Reg(r) = vloc {
                self.rstate[r.regidx()].0 = RegExt::Undefined;
                self.rstate[r.regidx()].1.push(iidx);
            }
        }
        self.istate[iidx] = vlocs.into_vec()
    }

    pub(super) fn is_alive(&self, iidx: InstIdx) -> bool {
        self.alive_until[iidx] != 0
    }

    /// Do the necessary actions at the (reverse) point of an instruction: expire any variables
    /// that are no longer alive; and ensure that constants (from `self.next_consts`) are in the
    /// correct registers.
    fn inst_entry(&mut self, be: &mut B, iidx: InstIdx) -> Result<(), CompilationError> {
        for x in &mut self.rstate {
            x.1.retain(|x| *x <= iidx);
        }

        for (reg, ext, tgt_bitw, kind) in self.next_consts.drain(..) {
            match ext {
                RegExt::Undefined | RegExt::Zeroed | RegExt::Ptr => {
                    be.zero_ext_const(reg, tgt_bitw, kind)?
                }
                RegExt::Signed => todo!(),
            }
        }

        Ok(())
    }

    /// Assign registers for the instruction at position `iidx`.
    pub(super) fn assign<const N: usize>(
        &mut self,
        be: &mut B,
        iidx: InstIdx,
        cnstrs: [RegCnstr<B::Reg>; N],
    ) -> Result<[B::Reg; N], CompilationError> {
        // There must be at most 1 output register.
        assert!(
            cnstrs
                .iter()
                .filter(|x| match x {
                    RegCnstr::InputOutput { .. } | RegCnstr::Output { .. } => true,
                    RegCnstr::Clobber { .. } | RegCnstr::Input { .. } | RegCnstr::Temp { .. } =>
                        false,
                })
                .count()
                <= 1
        );

        self.inst_entry(be, iidx)?;

        // Phase 1: Find registers for constraints. This phase does not mutate any state in `self.
        let mut allocs = [None; N];

        // Allocate all hard constraints i.e. those where only one register will do.
        for (i, cnstr) in cnstrs.iter().enumerate() {
            match cnstr {
                RegCnstr::Clobber { reg } => {
                    assert!(allocs[i].is_none());
                    allocs[i] = Some(*reg);
                }
                RegCnstr::Input { regs, .. }
                | RegCnstr::InputOutput { regs, .. }
                | RegCnstr::Output { regs, .. } => {
                    if regs.len() == 1 {
                        assert!(allocs[i].is_none());
                        allocs[i] = Some(regs[0]);
                    }
                }
                RegCnstr::Temp { .. } => (),
            }
        }

        // Try and allocate values to expected registers when possible.
        for (i, cnstr) in cnstrs.iter().enumerate() {
            if allocs[i].is_some() {
                continue;
            }
            match cnstr {
                RegCnstr::Clobber { .. } => unreachable!(),
                RegCnstr::Input { regs, .. }
                | RegCnstr::InputOutput { regs, .. }
                | RegCnstr::Output { regs, .. } => {
                    let find_iidx = match cnstr {
                        RegCnstr::Input { in_iidx, .. } => *in_iidx,
                        RegCnstr::InputOutput { .. } | RegCnstr::Output { .. } => iidx,
                        RegCnstr::Clobber { .. } | RegCnstr::Temp { .. } => todo!(),
                    };
                    if let Some(VarLoc::Reg(reg)) = self.istate[find_iidx]
                        .iter()
                        .find(|vloc| matches!(vloc, VarLoc::Reg(reg) if regs.contains(reg) && !allocs.contains(&Some(*reg))))
                    {
                        allocs[i] = Some(*reg);
                    };
                }
                RegCnstr::Temp { .. } => (),
            }
        }

        // For any remaining unallocated constraints, assign any valid register.
        for (i, cnstr) in cnstrs.iter().enumerate() {
            if allocs[i].is_some() {
                continue;
            }
            match cnstr {
                RegCnstr::Clobber { .. } => unreachable!(),
                RegCnstr::Input { regs, .. }
                | RegCnstr::InputOutput { regs, .. }
                | RegCnstr::Output { regs, .. }
                | RegCnstr::Temp { regs } => {
                    match regs.iter().find(|reg| !allocs.contains(&Some(**reg))) {
                        Some(reg) => allocs[i] = Some(*reg),
                        None => panic!("Cannot satisfy register constraints"),
                    }
                }
            }
        }

        // Phase 2: Now that every constraint has a register, update the system state to reflect
        // the new allocations. This updates state in `self`.
        for (reg, cnstr) in allocs.iter().map(|x| x.unwrap()).zip(cnstrs.into_iter()) {
            match cnstr {
                RegCnstr::Clobber { reg: _ } => {
                    self.ensure_unspilled(be, reg)?;
                }
                RegCnstr::Input {
                    in_iidx,
                    in_ext,
                    regs: _,
                    clobber,
                } => {
                    assert!(!clobber);
                    if iidx > self.alive_until[in_iidx] {
                        self.alive_until[in_iidx] = iidx;
                    }

                    if self.rstate[reg.regidx()].1.contains(&in_iidx) {
                        self.align_ext(reg, in_ext);
                    } else if let Inst::Const(Const { tyidx: _, kind }) = &self.b.inst(in_iidx) {
                        self.next_consts.push((
                            reg,
                            in_ext,
                            self.b.inst_bitw(self.m, in_iidx),
                            kind,
                        ));
                    } else {
                        self.ensure_unspilled(be, reg)?;
                        self.rstate[reg.regidx()] = (in_ext, smallvec![in_iidx]);
                        self.istate[in_iidx].push(VarLoc::Reg(reg));
                    }
                }
                RegCnstr::InputOutput {
                    in_iidx,
                    in_ext,
                    out_ext,
                    regs: _,
                } => {
                    if iidx > self.alive_until[in_iidx] {
                        self.alive_until[in_iidx] = iidx;
                    }

                    if self.rstate[reg.regidx()].1.contains(&iidx) {
                        self.rstate[reg.regidx()] = (in_ext, smallvec![in_iidx]);
                        self.istate[in_iidx].push(VarLoc::Reg(reg));
                        self.align_ext(reg, out_ext);
                    } else {
                        todo!();
                    }
                }
                RegCnstr::Output {
                    out_ext,
                    regs: _,
                    can_be_same_as_input,
                } => {
                    if self.rstate[reg.regidx()].1.contains(&iidx) {
                        self.align_ext(reg, out_ext);
                    } else {
                        todo!();
                    }
                }
                RegCnstr::Temp { regs: _ } => todo!(),
            }
        }

        Ok(allocs.map(|x| x.unwrap()))
    }

    fn align_ext(&mut self, reg: B::Reg, ext: RegExt) {
        // XXX
    }

    fn ensure_spilled(&mut self, be: &mut B, reg: B::Reg) -> Result<(), CompilationError> {
        todo!();
        // let (ext, iidxs) = &self.rstate[reg.regidx()];
        // for iidx in iidxs {
        //     let istate = &self.istate[*iidx];
        //     if istate.len() == 1 && matches!(istate[0], VarLoc::Reg(x) if x == reg) {
        //         let consumed =
        //             bg.spill(reg, *ext, self.b.inst_bitw(self.m, *iidx), self.stack_off)?;
        //         self.stack_off += consumed;
        //     }
        // }
        // Ok(())
    }

    fn ensure_unspilled(&mut self, be: &mut B, reg: B::Reg) -> Result<(), CompilationError> {
        let (ext, iidxs) = &self.rstate[reg.regidx()];
        for iidx in iidxs {
            todo!("{reg:?}");
        }
        Ok(())
    }
}

/// An abstraction of a register.
///
/// The register allocator knows almost nothing about registers except the following:
///
///   * Every register can be converted into a `RegIdx`. Registers must be numbered `0..n` where
///     `n` is the maximum number of registers in the system. As this suggests, the allocator needs
///     to consider registers as (sensible!) indexes.
pub(super) trait RegT: Clone + Copy + fmt::Debug + PartialEq {
    /// A register's index. Every register must be convertible into this type.
    type RegIdx: Idx;
    /// How many registers are available in this system?
    fn max_regidx() -> Self::RegIdx;
    /// What is this register's index?
    fn regidx(&self) -> Self::RegIdx;
}

#[derive(Clone, Debug)]
pub(super) struct VarLocs<Reg: RegT> {
    vlocs: SmallVec<[VarLoc<Reg>; 2]>,
}

impl<Reg: RegT> VarLocs<Reg> {
    pub(super) fn new(vlocs: SmallVec<[VarLoc<Reg>; 2]>) -> Self {
        Self { vlocs }
    }

    pub(super) fn into_vec(self) -> SmallVec<[VarLoc<Reg>; 2]> {
        self.vlocs
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &VarLoc<Reg>> {
        self.vlocs.iter()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum VarLoc<Reg> {
    /// The variable's value is stored on the stack at `off` bytes from the base pointer. Whether
    /// `off` is "above" or "below" the base pointer is system dependent.
    Stack(u32),
    /// The variable's value is a pointer into the stack (i.e. we're not reading a value from the
    /// stack: we're creating a pointer to a value on the stack). The pointer is `off` bytes from
    /// the base pointer. Whether `off` is "above" or "below" the base pointer is system dependent.
    StackPtr(u32),
    /// The variable's value is stored in a register.
    Reg(Reg),
    /// The variable's value is a constant.
    Const(ConstKind),
}

/// A register constraint. Each constraint leads to a single register being returned. Note: in some
/// situations (see the individual constraints), multiple constraints might return the same
/// register.
#[derive(Debug, PartialEq)]
pub(super) enum RegCnstr<'a, Reg: RegT> {
    /// This instruction clobbers `reg`.
    Clobber { reg: Reg },
    /// Make sure that `op` is loaded into a register drawn from `regs`, with its upper bits
    /// matching extension `in_ext`. If `clobber` is true, then the value in the register will be
    /// treated as clobbered on exit.
    Input {
        in_iidx: InstIdx,
        in_ext: RegExt,
        regs: &'a [Reg],
        clobber: bool,
    },
    /// Make sure that `op` is loaded into a register drawn from `regs`, with its upper bits
    /// matching extension `in_ext`; the result of the instruction will be in the same register
    /// with its upper bits matching extension `out_ext`.
    InputOutput {
        in_iidx: InstIdx,
        in_ext: RegExt,
        out_ext: RegExt,
        regs: &'a [Reg],
    },
    /// The result of the instruction will be in a register drawn from `regs` with its upper bits
    /// matching extension `out_ext`. If `can_be_same_as_input` is true, then the allocator may
    /// optionally return a register that is also used for an input (in such a case, the input will
    /// implicitly be considered clobbered).
    Output {
        out_ext: RegExt,
        regs: &'a [Reg],
        can_be_same_as_input: bool,
    },
    /// A temporary register drawn from `regs` that the instruction will clobber.
    Temp { regs: &'a [Reg] },
}

/// This `enum` serves two related purposes: it tells us what we know about the unused upper bits
/// of a value *and* it serves as a specification of what we want those values to be (in a
/// [RegCnstr]). What counts as "upper bits"?
///
///   * For normal values, we assume they may end up in a n-bit register: any bits between the
///     `bitw` of the type and n-bits are "upper bits". For max-bit values, the extension is
///     ignored, and can be set to any value.
///
///   * For floating point values, we assume that 32 bit floats and 64 bit doubles are not
///     intermixed. The extension is thus ignored.
///
///   * We do not currently support "non-normal / non-float" values (e.g. vector values) and will
///     have to think about those at a later point.
///
/// For example, if a 16 bit value is stored in a 64 bit value, we may know for sure that the upper
/// 48 bits are set to zero, or they sign extend the 16 bit value --- or we may have no idea!
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RegExt {
    /// We do not know what the upper bits are set to / we do not care what the upper bits are set
    /// to.
    Undefined,
    /// The upper bits zero extend the value / we want the upper bits to zero extend the value.
    Zeroed,
    /// The upper bits sign extend the value / we want the upper bits to sign extend the value.
    Signed,
    /// This value is a pointer. It is expected to fit into the register without needing to be zero
    /// or sign extended.
    Ptr,
}
