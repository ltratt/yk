//! AOT IR to HIR translator.

// FIXME!
#![allow(unused)]

use crate::{
    compile::{
        j2::hir,
        jitc_yk::{aot_ir::*, arbbitint::ArbBitInt},
        CompilationError, CompiledTrace,
    },
    log::stats::TimingState,
    mt::{TraceId, MT},
    trace::AOTTraceIterator,
    trace::TraceAction,
};
use index_vec::{index_vec, IndexSlice, IndexVec};
use smallvec::SmallVec;
use std::{
    assert_matches::assert_matches, collections::HashMap, ffi::CString, iter::Peekable, sync::Arc,
};

/// The symbol name of the global variable pointers array.
const GLOBAL_PTR_ARRAY_SYM: &str = "__yk_globalvar_ptrs";

pub(super) struct AotToHir {
    mt: Arc<MT>,
    /// The AOT IR.
    am: &'static Module,
    ta_iter: Peekable<TraceActionIterator>,
    trid: TraceId,
    bkind: BuildKind,
    promotions: Box<[u8]>,
    debug_strs: Vec<String>,
    coupler: Option<Arc<dyn CompiledTrace>>,
    /// The virtual address of the global variable pointer array.
    ///
    /// This is an array added to the LLVM AOT module by ykllvm containing a pointer to each global
    /// variable in the AOT module. The indices of the elements correspond with
    /// [aot_ir::GlobalDeclIdx]s. Note: this array is not available during testing, since tests are
    /// not built with ykllvm.
    globals: &'static [*const ()],
    tys: IndexVec<hir::TyIdx, hir::Ty>,
    /// Initially set to `None` until we find the locations for this trace's arguments.
    entry_safepoint_id: Option<u64>,
    frames: Vec<Frame>,
    func_decls: IndexVec<hir::FuncDeclIdx, hir::FuncDecl>,
    func_decl_map: HashMap<String, hir::FuncDeclIdx>,
    /// The JIT IR this struct builds.
    insts: Vec<hir::Inst>,
    guard_bodies: IndexVec<hir::GuardIdx, ()>,
}

impl AotToHir {
    pub(super) fn new(
        mt: &Arc<MT>,
        am: &'static Module,
        ta_iter: Box<dyn crate::trace::AOTTraceIterator>,
        trid: TraceId,
        bkind: BuildKind,
        promotions: Box<[u8]>,
        debug_strs: Vec<String>,
        coupler: Option<Arc<dyn CompiledTrace>>,
    ) -> Self {
        let globals = {
            let ptr = name_to_addr(GLOBAL_PTR_ARRAY_SYM).unwrap() as *const *const ();
            unsafe { std::slice::from_raw_parts(ptr, am.global_decls_len()) }
        };

        Self {
            mt: Arc::clone(mt),
            am,
            ta_iter: TraceActionIterator::new(ta_iter).peekable(),
            trid,
            bkind,
            promotions,
            debug_strs,
            coupler,
            globals,
            tys: IndexVec::new(),
            entry_safepoint_id: None,
            frames: Vec::new(),
            func_decls: IndexVec::new(),
            func_decl_map: HashMap::new(),
            insts: Vec::new(),
            guard_bodies: index_vec![],
        }
    }

    pub(super) fn build(mut self) -> Result<hir::Mod, CompilationError> {
        self.mt.stats.timing_state(TimingState::Compiling);

        // Get the control point blkid.
        let cp_blkid = match self.bkind {
            BuildKind::Loop | BuildKind::Coupler => {
                // The control point call will be found in the immediate predecessor of first block
                // we see. That means we first see a `MappedAOTBlock` with index `bb`...
                let TraceAction::MappedAOTBBlock { func_name, bb } =
                    self.ta_iter.next().unwrap()?
                else {
                    panic!()
                };
                assert!(bb > 0);
                // ...and the block we really want thus has index `bb -1`.
                self.ta_to_bid(&TraceAction::MappedAOTBBlock {
                    func_name,
                    bb: bb - 1,
                })
                .unwrap()
            }
            BuildKind::Guard => todo!(),
        };

        // Process the start of the trace.
        let body_iidx = match self.bkind {
            BuildKind::Loop => {
                let num_entry_vars = self.p_start_loop(&cp_blkid)?;
                self.skip_unmappable_blocks()?;
                num_entry_vars
            }
            BuildKind::Guard => todo!(),
            BuildKind::Coupler => todo!(),
        };

        while let Some(next) = self.ta_iter.next() {
            self.p_block(next?)?;
        }

        assert_eq!(self.frames.len(), 1);
        self.push_inst(
            hir::Exit {
                iidxs: self.frames[0]
                    .safepoint
                    .lives
                    .iter()
                    .map(|x| self.get_local(&x.to_inst_id()))
                    .collect::<Vec<_>>(),
            }
            .into(),
        );

        let entry = hir::Block {
            insts: IndexVec::from_vec(self.insts),
            entry_iidx: body_iidx,
            guard_bodies: self.guard_bodies,
        };
        let mk = match self.bkind {
            BuildKind::Loop => hir::ModKind::Loop {
                entry_safepoint_id: self.entry_safepoint_id.unwrap(),
                entry,
                body: None,
            },
            BuildKind::Guard => todo!(),
            BuildKind::Coupler => todo!(),
        };

        Ok(hir::Mod::new(self.trid, mk, self.func_decls, self.tys))
    }

    /// Skip unmappable blocks in `self.ta_iter`, leaving the iterator at the first mappable block.
    /// If any kind of error is encountered, this will return `Err`.
    fn skip_unmappable_blocks(&mut self) -> Result<(), CompilationError> {
        loop {
            let Some(next) = self.ta_iter.peek() else {
                todo!()
            };
            match next {
                Ok(ref x) => match x {
                    TraceAction::MappedAOTBBlock { .. } => return Ok(()),
                    TraceAction::UnmappableBBlock => (),
                    TraceAction::Promotion => todo!(),
                },
                Err(_) => {
                    return Err(self.ta_iter.next().unwrap().unwrap_err());
                }
            }
            let _ = self.ta_iter.next();
        }
    }

    fn peek_next_bbid(&mut self) -> Option<BBlockId> {
        self.ta_iter
            .peek()
            .and_then(|x| x.as_ref().ok())
            .cloned()
            .and_then(|x| self.ta_to_bid(&x))
    }

    fn const_to_iidx(
        &mut self,
        tyidx: hir::TyIdx,
        c: hir::ConstKind,
    ) -> Result<hir::InstIdx, CompilationError> {
        // We could, if we want, do some sort of caching for constants so that we don't end up with
        // as many duplicate instructions.
        self.push_inst(hir::Const::new(tyidx, c).into())
    }

    fn push_inst(&mut self, inst: hir::Inst) -> Result<hir::InstIdx, CompilationError> {
        let iidx = hir::InstIdx::from(self.insts.len());
        self.insts.push(inst);
        Ok(iidx)
    }

    /// This overwrites previous `(iid, inst)` mappings, which is necessary for unrolling to work.
    fn push_inst_and_link_local(
        &mut self,
        iid: InstId,
        inst: hir::Inst,
    ) -> Result<hir::InstIdx, CompilationError> {
        let iidx = hir::InstIdx::from(self.insts.len());
        self.frames.last_mut().unwrap().locals.insert(iid, iidx);
        self.insts.push(inst);
        Ok(iidx)
    }

    fn get_local(&self, instid: &InstId) -> hir::InstIdx {
        self.frames.last().unwrap().locals[instid]
    }

    fn push_func_decl(&mut self, func_decl: hir::FuncDecl) -> hir::FuncDeclIdx {
        let i = self.func_decls.len().into();
        self.func_decls.push(func_decl);
        i
    }

    fn push_ty(&mut self, ty: hir::Ty) -> Result<hir::TyIdx, CompilationError> {
        let i = hir::TyIdx::from(self.tys.len());
        self.tys.push(ty);
        Ok(i)
    }

    /// Translate a [TraceAction] to a [BBlockId]. If `ta` is not a mappable block, this will
    /// return `None`.
    fn ta_to_bid(&self, ta: &TraceAction) -> Option<BBlockId> {
        match ta {
            TraceAction::MappedAOTBBlock { func_name, bb } => {
                let fidx = self.am.funcidx(func_name.to_str().unwrap());
                if !self.am.func(fidx).is_declaration() {
                    Some(BBlockId::new(fidx, BBlockIdx::new(*bb)))
                } else {
                    None
                }
            }
            TraceAction::UnmappableBBlock => None,
            TraceAction::Promotion => unreachable!(),
        }
    }

    /// Process the start of a loop trace.
    fn p_start_loop(&mut self, cp_blkid: &BBlockId) -> Result<hir::InstIdx, CompilationError> {
        let cp_blk = self.am.bblock(cp_blkid);
        let inst = cp_blk
            .insts
            .iter()
            .find(|x| x.is_control_point(self.am))
            .unwrap();
        let safepoint = inst.safepoint().unwrap();
        assert!(self.frames.is_empty());
        self.frames.push(Frame {
            call: None,
            func: cp_blkid.funcidx(),
            safepoint,
            args: SmallVec::new(),
            locals: HashMap::new(),
        });

        self.entry_safepoint_id = Some(safepoint.id);
        let mut last_iidx = None;
        for (i, op) in safepoint.lives.iter().enumerate() {
            let ty = self.p_ty(op.type_(self.am))?;
            last_iidx = Some(self.push_inst_and_link_local(
                op.to_inst_id(),
                hir::LoadArg::new(ty, u32::try_from(i).unwrap()).into(),
            )?);
        }

        Ok(last_iidx.unwrap())
    }

    /// Process a type.
    fn p_ty(&mut self, ty: &Ty) -> Result<hir::TyIdx, CompilationError> {
        let ty = match ty {
            Ty::Void => hir::Ty::Void,
            Ty::Integer(x) => hir::Ty::Int(x.bitw()),
            Ty::Ptr => {
                // FIXME: AOT IR doesn't yet tell us what the address space is, so we guess "0".
                hir::Ty::Ptr(0)
            }
            Ty::Func(_ty) => todo!(),
            Ty::Struct(_ty) => todo!(),
            Ty::Float(_ty) => todo!(),
            Ty::Unimplemented(_) => todo!(),
        };
        self.push_ty(ty)
    }

    /// Process an [Operand] and return the [hir::InstIdx] it references. Note: this can insert
    /// instructions into [self.insts]!
    fn p_operand(&mut self, op: &Operand) -> Result<hir::InstIdx, CompilationError> {
        match op {
            Operand::Const(cidx) => {
                let c = self.am.const_(*cidx).unwrap_val();
                let bytes = c.bytes();
                match self.am.type_(c.tyidx()) {
                    Ty::Integer(x) => {
                        // FIXME: It would be better if the AOT IR had converted these integers in advance
                        // rather than doing this dance here.
                        let v = match x.bitw() {
                            1 | 8 => {
                                debug_assert_eq!(bytes.len(), 1);
                                u64::from(bytes[0])
                            }
                            16 => {
                                debug_assert_eq!(bytes.len(), 2);
                                u64::from(u16::from_ne_bytes([bytes[0], bytes[1]]))
                            }
                            32 => {
                                debug_assert_eq!(bytes.len(), 4);
                                u64::from(u32::from_ne_bytes([
                                    bytes[0], bytes[1], bytes[2], bytes[3],
                                ]))
                            }
                            64 => {
                                debug_assert_eq!(bytes.len(), 8);
                                u64::from_ne_bytes([
                                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
                                    bytes[6], bytes[7],
                                ])
                            }
                            _ => todo!("{}", x.bitw()),
                        };
                        let tyidx = self.push_ty(hir::Ty::Int(x.bitw()))?;
                        self.const_to_iidx(
                            tyidx,
                            hir::ConstKind::Int(ArbBitInt::from_u64(x.bitw(), v)),
                        )
                    }
                    Ty::Float(_) => {
                        todo!();
                        // let jit_tyidx = self.jit_mod.insert_ty(jit_ir::Ty::Float(fty.clone()))?;
                        // // unwrap cannot fail if the AOT IR is valid.
                        // let val = f64::from_ne_bytes(bytes[0..8].try_into().unwrap());
                        // Ok(jit_ir::Const::Float(jit_tyidx, val))
                    }
                    Ty::Ptr => {
                        todo!();
                        // let val: usize;
                        // #[cfg(target_arch = "x86_64")]
                        // {
                        //     debug_assert_eq!(bytes.len(), 8);
                        //     val = usize::from_ne_bytes([
                        //         bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
                        //         bytes[6], bytes[7],
                        //     ]);
                        // }
                        // Ok(jit_ir::Const::Ptr(val))
                    }
                    x => todo!("{x:?}"),
                }
            }
            Operand::Local(iid) => Ok(self.get_local(iid)),
            Operand::Global(gidx) => {
                let gl = self.am.global_decl(*gidx);
                if gl.is_threadlocal() {
                    todo!();
                } else {
                    let tyidx = self.push_ty(hir::Ty::Ptr(0))?;
                    self.const_to_iidx(
                        tyidx,
                        hir::ConstKind::Ptr(self.globals[usize::from(*gidx)].addr()),
                    )
                }
            }
            Operand::Func(_fidx) => todo!(),
        }
    }

    fn p_block(&mut self, ta: TraceAction) -> Result<(), CompilationError> {
        let bid = self.ta_to_bid(&ta).unwrap();
        let blk = self.am.bblock(&bid);
        for (i, inst) in blk.insts.iter().enumerate() {
            let iid = InstId::new(bid.funcidx(), bid.bbidx(), BBlockInstIdx::new(i));
            match inst {
                Inst::Nop => todo!(),
                Inst::Load { .. } => self.p_load(iid, inst)?,
                Inst::Store { .. } => self.p_store(iid, inst)?,
                Inst::Alloca {
                    tyidx,
                    count,
                    align,
                } => todo!(),
                Inst::Call {
                    callee,
                    args,
                    safepoint,
                } => {
                    if inst.is_control_point(self.am) {
                        // If we encounter a control point call, it should be the final thing we
                        // see.
                        assert!(self.peek_next_bbid().is_none());
                        break;
                    }
                    self.p_call(iid, inst)?
                }
                Inst::Br { .. } => (),
                Inst::CondBr {
                    cond,
                    true_bb,
                    false_bb,
                    safepoint,
                } => self.p_condbr(iid, inst)?,
                Inst::ICmp {
                    tyidx,
                    lhs,
                    pred,
                    rhs,
                } => self.p_icmp(iid, inst)?,
                Inst::Ret { val } => todo!(),
                Inst::InsertValue { agg, elem } => todo!(),
                Inst::PtrAdd {
                    tyidx,
                    ptr,
                    const_off,
                    dyn_elem_counts,
                    dyn_elem_sizes,
                } => todo!(),
                Inst::BinaryOp { lhs, binop, rhs } => self.p_binop(iid, inst)?,
                Inst::Cast {
                    cast_kind,
                    val,
                    dest_tyidx,
                } => todo!(),
                Inst::Switch {
                    test_val,
                    default_dest,
                    case_values,
                    case_dests,
                    safepoint,
                } => todo!(),
                Inst::Phi {
                    tyidx,
                    incoming_bbs,
                    incoming_vals,
                } => todo!(),
                Inst::IndirectCall {
                    ftyidx,
                    callop,
                    args,
                    safepoint,
                } => todo!(),
                Inst::Select {
                    cond,
                    trueval,
                    falseval,
                } => todo!(),
                Inst::LoadArg { arg_idx, ty_idx } => todo!(),
                Inst::FCmp {
                    tyidx,
                    lhs,
                    pred,
                    rhs,
                } => todo!(),
                Inst::Promote {
                    tyidx,
                    val,
                    safepoint,
                } => todo!(),
                Inst::FNeg { val } => todo!(),
                Inst::DebugStr { msg } => todo!(),
                Inst::IdempotentPromote { tyidx, val } => todo!(),
                Inst::Unimplemented {
                    tyidx,
                    llvm_inst_str,
                } => todo!(),
            }
        }
        Ok(())
    }

    fn p_binop(&mut self, iid: InstId, inst: &Inst) -> Result<(), CompilationError> {
        let Inst::BinaryOp { lhs, binop, rhs } = inst else {
            panic!()
        };
        let lhs = self.p_operand(lhs)?;
        let rhs = self.p_operand(rhs)?;
        let inst = match binop {
            BinOp::Add => hir::Add::new(
                self.p_ty(inst.def_type(self.am).unwrap())?,
                lhs,
                rhs,
                false,
                false,
            ),
            BinOp::Sub => todo!(),
            BinOp::Mul => todo!(),
            BinOp::Or => todo!(),
            BinOp::And => todo!(),
            BinOp::Xor => todo!(),
            BinOp::Shl => todo!(),
            BinOp::AShr => todo!(),
            BinOp::FAdd => todo!(),
            BinOp::FDiv => todo!(),
            BinOp::FMul => todo!(),
            BinOp::FRem => todo!(),
            BinOp::FSub => todo!(),
            BinOp::LShr => todo!(),
            BinOp::SDiv => todo!(),
            BinOp::SRem => todo!(),
            BinOp::UDiv => todo!(),
            BinOp::URem => todo!(),
        };
        self.push_inst_and_link_local(iid, inst.into()).map(|_| ())
    }

    fn p_call(&mut self, iid: InstId, inst: &Inst) -> Result<(), CompilationError> {
        let Inst::Call {
            callee,
            args,
            safepoint,
        } = inst
        else {
            panic!()
        };

        // Ignore LLVM debug calls.
        if inst.is_debug_call(self.am) {
            return Ok(());
        }
        // Ignore calls the software tracer makes to record blocks.
        #[cfg(tracer_swt)]
        if AOT_MOD.func(*callee).name() == "__yk_trace_basicblock" {
            return Ok(());
        }

        let mut jargs = SmallVec::with_capacity(args.len());
        for x in args {
            jargs.push(self.p_operand(x)?);
        }

        let func = self.am.func(*callee);
        if !func.is_declaration()
            && !func.is_outline()
            && !func.is_idempotent()
            && !func.contains_call_to(self.am, "llvm.va_start")
        {
            todo!();
        } else {
            // Unmappable call.
            let fdidx = if let Some(x) = self.func_decl_map.get(func.name()) {
                *x
            } else {
                let Ty::Func(jfty) = self.am.type_(func.tyidx()) else {
                    panic!()
                };
                let rtn_ty = self.p_ty(self.am.type_(jfty.ret_ty()))?;
                let mut jargs_tys = SmallVec::with_capacity(jfty.arg_tyidxs().len());
                for arg_ty in jfty.arg_tyidxs() {
                    jargs_tys.push(self.p_ty(self.am.type_(*arg_ty))?);
                }
                let fty = hir::FuncTy::new(rtn_ty, jargs_tys, jfty.is_vararg());
                let fdecl = hir::FuncDecl::new(func.name().to_owned(), fty);
                let fdidx = self.push_func_decl(fdecl);
                self.func_decl_map.insert(func.name().to_owned(), fdidx);
                fdidx
            };
            let tyidx = self.push_ty(hir::Ty::Ptr(0))?;
            let fdecl = &self.func_decls[usize::from(fdidx)];
            let addr = name_to_addr(func.name())?;
            let addr_iidx = self.const_to_iidx(tyidx, hir::ConstKind::Ptr(addr))?;
            self.push_inst_and_link_local(iid, hir::Call::new(addr_iidx, fdidx, jargs).into());
        }
        Ok(())
    }

    fn p_condbr(&mut self, iid: InstId, inst: &Inst) -> Result<(), CompilationError> {
        let Inst::CondBr {
            cond,
            true_bb,
            false_bb,
            safepoint,
        } = inst
        else {
            panic!()
        };

        let next_bb = self.peek_next_bbid().unwrap();
        assert_eq!(
            next_bb.funcidx(),
            iid.funcidx(),
            "Control flow has diverged"
        );
        assert!(next_bb.bbidx() == *true_bb || next_bb.bbidx() == *false_bb);

        let vars = safepoint
            .lives
            .iter()
            .map(|x| (x.to_inst_id(), self.get_local(&x.to_inst_id())))
            .collect::<Vec<_>>();
        let hinst = hir::Guard {
            expect_true: next_bb.bbidx() == *true_bb,
            cond: self.p_operand(cond)?,
            vars,
        };
        self.guard_bodies.push(());
        self.push_inst_and_link_local(iid, hinst.into()).map(|_| ())
    }

    fn p_icmp(&mut self, iid: InstId, inst: &Inst) -> Result<(), CompilationError> {
        let Inst::ICmp {
            tyidx,
            lhs,
            pred,
            rhs,
        } = inst
        else {
            panic!()
        };
        assert_eq!(self.am.type_(*tyidx).bitw(), 1);
        let tyidx = self.push_ty(hir::Ty::Int(1))?;
        let lhs = self.p_operand(lhs)?;
        let rhs = self.p_operand(rhs)?;
        let kind = match pred {
            Predicate::Equal => hir::ICmpKind::Eq,
            Predicate::NotEqual => hir::ICmpKind::Ne,
            Predicate::UnsignedGreater => hir::ICmpKind::Ugt,
            Predicate::UnsignedGreaterEqual => hir::ICmpKind::Uge,
            Predicate::UnsignedLess => hir::ICmpKind::Ult,
            Predicate::UnsignedLessEqual => hir::ICmpKind::Ule,
            Predicate::SignedGreater => hir::ICmpKind::Sgt,
            Predicate::SignedGreaterEqual => hir::ICmpKind::Sge,
            Predicate::SignedLess => hir::ICmpKind::Slt,
            Predicate::SignedLessEqual => hir::ICmpKind::Sle,
        };
        self.push_inst_and_link_local(
            iid,
            hir::ICmp {
                tyidx,
                kind,
                lhs,
                rhs,
                samesign: false,
            }
            .into(),
        )
        .map(|_| ())
    }

    fn p_load(&mut self, iid: InstId, inst: &Inst) -> Result<(), CompilationError> {
        let Inst::Load {
            ptr,
            tyidx,
            volatile,
        } = inst
        else {
            panic!()
        };
        let ty = self.p_ty(self.am.type_(*tyidx))?;
        let ptr = self.p_operand(ptr)?;
        self.push_inst_and_link_local(iid, hir::Load::new(ty, ptr, *volatile).into())
            .map(|_| ())
    }

    fn p_store(&mut self, iid: InstId, inst: &Inst) -> Result<(), CompilationError> {
        let Inst::Store { val, tgt, volatile } = inst else {
            panic!()
        };
        let ptr = self.p_operand(tgt)?;
        let val = self.p_operand(val)?;
        self.push_inst_and_link_local(iid, hir::Store::new(ptr, val, *volatile).into())
            .map(|_| ())
    }
}

pub(super) enum BuildKind {
    Loop,
    Guard,
    Coupler,
}

/// An inlined frame.
#[derive(Debug)]
struct Frame {
    call: Option<InstId>,
    func: FuncIdx,
    safepoint: &'static DeoptSafepoint,
    args: SmallVec<[hir::InstIdx; 4]>,
    locals: HashMap<InstId, hir::InstIdx>,
}

struct TraceActionIterator {
    ta_iter: Peekable<Box<dyn crate::trace::AOTTraceIterator>>,
}

impl TraceActionIterator {
    fn new(ta_iter: Box<dyn crate::trace::AOTTraceIterator>) -> Self {
        Self {
            ta_iter: ta_iter.peekable(),
        }
    }
}

impl Iterator for TraceActionIterator {
    type Item = Result<TraceAction, CompilationError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.ta_iter
            .next()
            .map(|x| x.map_err(|e| CompilationError::General(e.to_string())))
    }
}

/// dlsym
fn name_to_addr(n: &str) -> Result<usize, CompilationError> {
    let Ok(cn) = CString::new(n) else { todo!() };
    let rtn = unsafe { libc::dlsym(std::ptr::null_mut(), cn.as_c_str().as_ptr()) };
    if !rtn.is_null() {
        Ok(rtn.addr())
    } else {
        todo!("{n}")
    }
}
