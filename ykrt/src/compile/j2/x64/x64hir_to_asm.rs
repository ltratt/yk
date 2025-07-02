use crate::{
    aotsmp::AOT_STACKMAPS,
    compile::{
        j2::{
            hir::*,
            hir_to_asm::HirToAsmBackend,
            regalloc::{RegAlloc, RegCnstr, RegExt, VarLoc, VarLocs},
            x64::{
                asm::{Asm, LabelIdx},
                x64regalloc::{Reg, ALL_XMM_REGS, NORMAL_GP_REGS},
                X64CompiledTrace,
            },
        },
        CompilationError, CompiledTrace,
    },
    mt::TraceId,
};
use array_concat::concat_arrays;
use iced_x86::{Code, Instruction as X64Inst, MemoryOperand, Register};
use index_vec::{index_vec, IndexVec};
use smallvec::{smallvec, SmallVec};
use std::{assert_matches::debug_assert_matches, sync::Arc};

pub(in crate::compile::j2) struct X64HirToAsm<'a> {
    m: &'a Mod,
    asm: Asm,
}

impl<'a> X64HirToAsm<'a> {
    pub(in crate::compile::j2) fn new(m: &'a Mod) -> Self {
        Self { m, asm: Asm::new() }
    }
}

impl HirToAsmBackend for X64HirToAsm<'_> {
    type Label = LabelIdx;
    type Reg = Reg;

    /// Convert a stackmap [Location](s) to a pair `([VarLoc], spill)`. If `VarLoc` is a register and
    /// `spill` is `Some`, then the value has previously been spilt to `rbp-spill.unwrap()`. If `None`,
    /// this value is only in the register.
    fn smp_to_vloc(smp_locs: &SmallVec<[yksmp::Location; 1]>) -> VarLocs<Self::Reg> {
        fn dwarf_reg_to_vloc(dwarf_reg: u16) -> VarLoc<Reg> {
            VarLoc::Reg(match dwarf_reg {
                0 => Reg::RAX,
                1 => Reg::RDX,
                2 => Reg::RCX,
                3 => Reg::RBX,
                4 => Reg::RSI,
                5 => Reg::RDI,
                6 => unreachable!(), // RBP
                7 => unreachable!(), // RSP
                8 => Reg::R8,
                9 => Reg::R9,
                10 => Reg::R10,
                11 => Reg::R11,
                12 => Reg::R12,
                13 => Reg::R13,
                14 => Reg::R14,
                15 => Reg::R15,
                _ => unreachable!(),
            })
        }

        use yksmp::Location as L;
        assert_eq!(smp_locs.len(), 1, "Multi-locations not yet supported");
        match &smp_locs[0] {
            L::Register(dwarf_reg, _sz, extras) => {
                let mut out = smallvec![dwarf_reg_to_vloc(*dwarf_reg)];
                for x in extras {
                    if *x >= 0 {
                        out.push(dwarf_reg_to_vloc(x.cast_unsigned()));
                    } else {
                        out.push(VarLoc::Stack(u32::from(x.unsigned_abs())));
                    }
                }
                VarLocs::new(out)
            }
            L::Direct(6, off, _sz) => {
                assert!(*off <= 0);
                VarLocs::new(smallvec![VarLoc::Stack(off.unsigned_abs())])
            }
            L::Indirect(6, off, _sz) => {
                assert!(*off <= 0);
                VarLocs::new(smallvec![VarLoc::StackPtr(off.unsigned_abs())])
            }
            // L::Constant(v) => {
            //     let bitw = m.inst(iidx).def_bitw(m);
            //     assert!(bitw <= 32);
            //     VarLoc::ConstInt {
            //         bits: bitw,
            //         v: u64::from(*v),
            //     }
            // }
            // L::LargeConstant(v) => {
            //     let bitw = m.inst(iidx).def_bitw(m);
            //     assert!(bitw <= 64);
            //     VarLoc::ConstInt { bits: bitw, v: *v }
            // }
            e => {
                todo!("{:?}", e);
            }
        }
    }

    fn mk_label(&mut self) -> Self::Label {
        self.asm.new_br_label()
    }

    fn set_label(&mut self, label: Self::Label) {
        self.asm.set_br_label(label);
    }

    fn block_completed(&mut self) {
        self.asm.block_completed();
    }

    fn into_exe(self, trid: TraceId) -> Result<Arc<dyn CompiledTrace>, CompilationError> {
        Ok(Arc::new(self.asm.into_exe(trid)))
    }

    fn loop_backwards_jump(&mut self) -> Result<(), CompilationError> {
        self.asm
            .push_inst(X64Inst::with_branch(Code::Jmp_rel32_64, 0));
        Ok(())
    }

    fn guard_exit(&mut self) -> Result<(), CompilationError> {
        self.asm.push_inst(Ok(X64Inst::with(Code::Ud2)));
        Ok(())
    }

    fn zero_ext_const(
        &mut self,
        reg: Reg,
        tgt_bitw: u32,
        kind: &ConstKind,
    ) -> Result<(), CompilationError> {
        match kind {
            ConstKind::Int(x) => {
                assert!(tgt_bitw >= x.bitw());
                match tgt_bitw {
                    32 => {
                        if let Some(x) = x.to_zero_ext_u32() {
                            self.asm.push_inst(X64Inst::with2(
                                Code::Mov_r32_imm32,
                                reg.to_reg32(),
                                x,
                            ));
                        } else {
                            todo!();
                        }
                    }
                    x => todo!("{x}"),
                }
            }
            ConstKind::Ptr(x) => {
                assert_eq!(tgt_bitw, 64);
                self.asm.push_inst(X64Inst::with2(
                    Code::Mov_r64_imm64,
                    reg.to_reg64(),
                    u64::try_from(*x).unwrap(),
                ));
            }
        }
        Ok(())
    }

    fn spill(
        &mut self,
        reg: Reg,
        ext: RegExt,
        bitw: u32,
        mut stack_off: usize,
    ) -> Result<usize, CompilationError> {
        match reg {
            Reg::RAX
            | Reg::RCX
            | Reg::RDX
            | Reg::RBX
            | Reg::RSI
            | Reg::RDI
            | Reg::R8
            | Reg::R9
            | Reg::R10
            | Reg::R11
            | Reg::R12
            | Reg::R13
            | Reg::R14
            | Reg::R15 => {
                match bitw {
                    32 => {
                        stack_off = stack_off.next_multiple_of(8);
                        todo!();
                        // self.push_inst(X64Inst::new(Code::Movsxd_r32_rm32%
                    }
                    x => todo!("{x}"), // stack_off = stack_off.next_multiple_of(8);
                }
            }
            x => todo!("{x:?}"),
        }
        Ok(stack_off)
    }

    fn i_add(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        Add {
            tyidx,
            lhs,
            rhs,
            nuw,
            nsw,
        }: &Add,
    ) -> Result<(), CompilationError> {
        // We don't handle nuw or nsw yet.
        assert!(!*nuw && !*nsw);
        let lhs_bitw = b.inst_bitw(self.m, *lhs);
        assert_eq!(lhs_bitw, b.inst_bitw(self.m, *rhs));
        let in_ext = match lhs_bitw {
            32 | 64 => RegExt::Undefined,
            x => todo!("{x}"),
        };
        let [lhsr, rhsr] = ra.assign(
            self,
            iidx,
            [
                RegCnstr::InputOutput {
                    in_iidx: *lhs,
                    in_ext,
                    out_ext: RegExt::Zeroed,
                    regs: &NORMAL_GP_REGS,
                },
                RegCnstr::Input {
                    in_iidx: *rhs,
                    in_ext,
                    regs: &NORMAL_GP_REGS,
                    clobber: false,
                },
            ],
        )?;

        match lhs_bitw {
            32 => {
                self.asm.push_inst(X64Inst::with2(
                    Code::Add_rm32_r32,
                    lhsr.to_reg32(),
                    rhsr.to_reg32(),
                ));
            }
            x => todo!("{x}"),
        }
        Ok(())
    }

    fn i_call(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        Call { tgt, fdclidx, args }: &Call,
    ) -> Result<(), CompilationError> {
        // Calls on x64 with the SysV ABI have complex requirements. Some GP registers have special
        // meanings in some, but not necessarily all, cases and some, but not all, GP registers are
        // preserved across call. FP registers are much simpler: there are no special meanings, and
        // no FP registers are preserved across calls.
        //
        // In essence, we build up constraints for every GP register that's part of the ABI and all
        // FP registers. We start by assuming those registers are clobbered, and gradually refine
        // them with more precise constraints as needed.

        // The GP registers we will clobber.
        //
        // NOTE! The order these are stored in is relied upon by `GP_CLOBBER_TMPS`, `RAX_OFF` and
        // `GP_ARGS`. Changing this order requires updating those variables too.
        const GP_CLOBBERS: [Reg; 9] = [
            Reg::RAX,
            Reg::RCX,
            Reg::RDX,
            Reg::RSI,
            Reg::RDI,
            Reg::R8,
            Reg::R9,
            Reg::R10,
            Reg::R11,
        ];
        // This is a sort-of-hack to assuage the borrow checker. The order of elements must exactly
        // match `GB_CLOBBERS`.
        const GP_CLOBBER_TMPS: [&[Reg]; 9] = [
            &[Reg::RAX],
            &[Reg::RCX],
            &[Reg::RDX],
            &[Reg::RSI],
            &[Reg::RDI],
            &[Reg::R8],
            &[Reg::R9],
            &[Reg::R10],
            &[Reg::R11],
        ];
        // What offset is `Reg::RAX` in `GP_CLOBBERS`?
        const RAX_OFF: usize = 0;
        // The order that GP parameters should be passed in: each entry is an offset into
        // `GP_CLOBBERS`.
        const GP_ARG_OFFS: [usize; 6] = [
            4, // RDI
            3, // RSI
            2, // RDX
            1, // RCX
            5, // R8
            6, // R9
        ];

        let mut gp_cnstrs: [_; 9] = GP_CLOBBERS.map(|x| RegCnstr::Clobber { reg: x });
        let fp_cnstrs: [_; 16] = ALL_XMM_REGS.map(|x| RegCnstr::Clobber { reg: x });
        let num_float_args = 0;

        let mut gp_iter = GP_ARG_OFFS.iter();
        for arg in args {
            let arg_ty = b.inst_ty(self.m, *arg);
            match arg_ty {
                Ty::Func(_) => todo!(),
                Ty::Int(_) | Ty::Ptr(_) => {
                    let in_ext = match arg_ty {
                        Ty::Func(_) | Ty::Void => unreachable!(),
                        Ty::Int(_) => RegExt::Zeroed,
                        Ty::Ptr(_) => RegExt::Ptr,
                    };
                    let gp_off = gp_iter.next().unwrap();
                    debug_assert_matches!(gp_cnstrs[*gp_off], RegCnstr::Clobber { .. });
                    gp_cnstrs[*gp_off] = RegCnstr::Input {
                        in_iidx: *arg,
                        in_ext,
                        regs: GP_CLOBBER_TMPS[*gp_off],
                        clobber: false,
                    };
                }
                Ty::Void => todo!(),
            }
        }

        let fty = &self.m.func_decls[*fdclidx].fty;
        match self.m.ty(fty.rtn_tyidx) {
            Ty::Func(func_ty) => todo!(),
            Ty::Int(_) => {
                debug_assert_matches!(gp_cnstrs[RAX_OFF], RegCnstr::Clobber { .. });
                if !fty.has_varargs {
                    // RAX isn't used as an input for non-varargs functions.
                    // gp_cnstrs[RAX_OFF] = RegCnstr::InputOutput {
                    //     iidx: *tgt,
                    //     in_ext: RegExt::Zeroed,
                    //     out_ext: RegExt::Zeroed,
                    //     regs: GP_CLOBBER_TMPS[RAX_OFF],
                    // };
                    todo!();
                }
            }
            Ty::Ptr(_) => todo!(),
            Ty::Void => todo!(),
        }

        if !fty.has_varargs {
            todo!();
        } else {
            let cnstrs: [_; GP_CLOBBERS.len() + 1] = concat_arrays!(
                gp_cnstrs,
                [RegCnstr::Input {
                    in_iidx: *tgt,
                    in_ext: RegExt::Ptr,
                    regs: &NORMAL_GP_REGS,
                    clobber: false
                }]
            );
            let [.., tgtr] = ra.assign(self, iidx, cnstrs)?;
            self.asm
                .push_inst(X64Inst::with1(Code::Call_rm64, tgtr.to_reg64()));
            self.asm.push_inst(X64Inst::with2(
                Code::Mov_r32_imm32,
                Register::EAX,
                num_float_args,
            ));
        }
        Ok(())
    }

    fn i_guard(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        gidx: GuardIdx,
        Guard {
            expect_true,
            cond,
            vars,
        }: &Guard,
    ) -> Result<(), CompilationError> {
        let [cndr] = ra.assign(
            self,
            iidx,
            [RegCnstr::Input {
                in_iidx: *cond,
                in_ext: RegExt::Undefined,
                regs: &NORMAL_GP_REGS,
                clobber: false,
            }],
        )?;
        self.asm.push_inst(if *expect_true {
            X64Inst::with_branch(Code::Jae_rel32_64, 0)
        } else {
            X64Inst::with_branch(Code::Jb_rel32_64, 0)
        });
        self.asm
            .push_inst(X64Inst::with2(Code::Bt_rm32_imm8, cndr.to_reg32(), 0));
        Ok(())
    }

    fn i_icmp(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        ICmp {
            tyidx,
            kind,
            lhs,
            rhs,
            samesign,
        }: &ICmp,
    ) -> Result<(), CompilationError> {
        let lhs_bitw = b.inst_bitw(self.m, *lhs);
        assert_eq!(lhs_bitw, b.inst_bitw(self.m, *rhs));
        let in_ext = match lhs_bitw {
            32 | 64 => RegExt::Undefined,
            x => todo!("{x}"),
        };
        let [lhsr, rhsr, outr] = ra.assign(
            self,
            iidx,
            [
                RegCnstr::Input {
                    in_iidx: *lhs,
                    in_ext,
                    regs: &NORMAL_GP_REGS,
                    clobber: false,
                },
                RegCnstr::Input {
                    in_iidx: *rhs,
                    in_ext,
                    regs: &NORMAL_GP_REGS,
                    clobber: false,
                },
                RegCnstr::Output {
                    out_ext: RegExt::Undefined,
                    regs: &NORMAL_GP_REGS,
                    can_be_same_as_input: true,
                },
            ],
        )?;

        let c = match kind {
            ICmpKind::Eq => Code::Sete_rm8,
            ICmpKind::Ne => Code::Setne_rm8,
            ICmpKind::Ugt => Code::Seta_rm8,
            ICmpKind::Uge => Code::Setae_rm8,
            ICmpKind::Ult => Code::Setb_rm8,
            ICmpKind::Ule => Code::Setbe_rm8,
            ICmpKind::Sgt => Code::Setg_rm8,
            ICmpKind::Sge => Code::Setge_rm8,
            ICmpKind::Slt => Code::Setl_rm8,
            ICmpKind::Sle => Code::Setle_rm8,
        };
        self.asm.push_inst(X64Inst::with1(c, outr.to_reg8()));

        match lhs_bitw {
            32 => {
                self.asm.push_inst(X64Inst::with2(
                    Code::Cmp_rm32_r32,
                    lhsr.to_reg32(),
                    rhsr.to_reg32(),
                ));
            }
            x => todo!("{x}"),
        }
        Ok(())
    }

    fn i_load(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        Load {
            tyidx,
            ptr,
            is_volatile: _,
        }: &Load,
    ) -> Result<(), CompilationError> {
        let [ptrr, outr] = ra.assign(
            self,
            iidx,
            [
                RegCnstr::Input {
                    in_iidx: *ptr,
                    in_ext: RegExt::Ptr,
                    regs: &NORMAL_GP_REGS,
                    clobber: false,
                },
                RegCnstr::Output {
                    out_ext: RegExt::Zeroed,
                    regs: &NORMAL_GP_REGS,
                    can_be_same_as_input: true,
                },
            ],
        )?;

        match self.m.ty(*tyidx) {
            Ty::Func(_) => todo!(),
            Ty::Int(bitw) => match bitw {
                32 => {
                    self.asm.push_inst(X64Inst::with2(
                        Code::Mov_r32_rm32,
                        outr.to_reg32(),
                        MemoryOperand::with_base(ptrr.to_reg64()),
                    ));
                }
                x => todo!("{x}"),
            },
            Ty::Ptr(_) => {
                self.asm.push_inst(X64Inst::with2(
                    Code::Mov_r64_rm64,
                    outr.to_reg64(),
                    MemoryOperand::with_base(ptrr.to_reg64()),
                ));
            }
            Ty::Void => todo!(),
        }
        Ok(())
    }

    fn i_store(
        &mut self,
        ra: &mut RegAlloc<Self>,
        b: &Block,
        iidx: InstIdx,
        Store {
            ptr,
            val,
            is_volatile,
        }: &Store,
    ) -> Result<(), CompilationError> {
        let [ptrr, valr] = ra.assign(
            self,
            iidx,
            [
                RegCnstr::Input {
                    in_iidx: *ptr,
                    in_ext: RegExt::Ptr,
                    regs: &NORMAL_GP_REGS,
                    clobber: false,
                },
                RegCnstr::Input {
                    in_iidx: *val,
                    in_ext: RegExt::Ptr,
                    regs: &NORMAL_GP_REGS,
                    clobber: false,
                },
            ],
        )?;

        match self.m.ty(b.inst(*val).tyidx(self.m)) {
            Ty::Func(_) => todo!(),
            Ty::Int(bitw) => match bitw {
                32 => {
                    self.asm.push_inst(X64Inst::with2(
                        Code::Mov_rm32_r32,
                        MemoryOperand::with_base(ptrr.to_reg64()),
                        valr.to_reg32(),
                    ));
                }
                x => todo!("{x}"),
            },
            Ty::Ptr(_) => todo!(),
            Ty::Void => todo!(),
        }
        Ok(())
    }
}
