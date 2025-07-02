//! High-level trace IR (HIR).

use crate::{
    compile::jitc_yk::{aot_ir, arbbitint::ArbBitInt},
    mt::TraceId,
};
use declarative_enum_dispatch::enum_dispatch;
use index_vec::IndexVec;
use smallvec::SmallVec;

#[derive(Debug)]
pub(super) struct Mod {
    pub trid: TraceId,
    pub kind: ModKind,
    pub func_decls: IndexVec<FuncDeclIdx, FuncDecl>,
    pub tys: IndexVec<TyIdx, Ty>,
}

impl Mod {
    pub(super) fn new(
        trid: TraceId,
        kind: ModKind,
        func_decls: IndexVec<FuncDeclIdx, FuncDecl>,
        tys: IndexVec<TyIdx, Ty>,
    ) -> Self {
        Self {
            trid,
            kind,
            func_decls,
            tys,
        }
    }

    pub(super) fn ty(&self, tyidx: TyIdx) -> &Ty {
        &self.tys[tyidx]
    }
}

#[derive(Debug)]
pub(super) enum ModKind {
    Loop {
        entry_safepoint_id: u64,
        entry: Block,
        body: Option<Block>,
    },
    Guard {
        entry: Block,
    },
    Coupler {
        entry: Block,
    },
}

/// An ordered sequence of instructions. Formally this is not a "basic" block since it can, and
/// generally does, contain one or more guards.
#[derive(Debug)]
pub(super) struct Block {
    /// The main sequence of instructions constituting this block. The last instruction is
    /// guaranteed to be an [Exit] instruction.
    pub insts: IndexVec<InstIdx, Inst>,
    /// The index of the first non-entry instruction; by definition, indices 0 (inc)..`body_iidx`
    /// (exc.) are guaranteed to be the "there's nothing to execute here" entry variables.
    pub entry_iidx: InstIdx,
    /// Zero or more guard bodies.
    pub guard_bodies: IndexVec<GuardIdx, ()>,
}

impl Block {
    pub(super) fn inst(&self, idx: InstIdx) -> &Inst {
        &self.insts[usize::from(idx)]
    }

    pub(super) fn last_inst(&self) -> &Inst {
        self.insts.last().unwrap()
    }

    pub(super) fn insts_iter(&self) -> impl DoubleEndedIterator<Item = (InstIdx, &Inst)> + '_ {
        self.insts.iter_enumerated()
    }

    /// Return the bit width of the instruction `iidx`. This is a convenience function over other
    /// public functions.
    pub(super) fn inst_bitw(&self, m: &Mod, iidx: InstIdx) -> u32 {
        m.ty(self.inst(iidx).tyidx(m)).bitw()
    }

    /// Return the bit width of the instruction `iidx`. This is a convenience function over other
    /// public functions.
    pub(super) fn inst_ty<'a>(&self, m: &'a Mod, iidx: InstIdx) -> &'a Ty {
        m.ty(self.inst(iidx).tyidx(m))
    }

    pub(super) fn exit_iidxs(&self) -> &[InstIdx] {
        let Inst::Exit(Exit { iidxs }) = self.last_inst() else {
            panic!()
        };
        iidxs
    }
}

#[derive(Debug)]
pub(super) enum Ty {
    Func(Box<FuncTy>),
    // An integer `u32` bits wide, where `u > 0 && u <= 24`.
    Int(u32),
    /// A pointer in an address space. LLVM allows 24 bits to be used.
    Ptr(u32),
    Void,
}

impl Ty {
    pub(super) fn bitw(&self) -> u32 {
        match self {
            Ty::Func(_func_ty) => todo!(),
            Ty::Int(bitw) => *bitw,
            Ty::Ptr(addrspace) => {
                assert_eq!(*addrspace, 0);
                #[cfg(target_arch = "x86_64")]
                64
            }
            Ty::Void => todo!(),
        }
    }
}

#[derive(Debug)]
pub(super) struct FuncTy {
    pub rtn_tyidx: TyIdx,
    pub arg_tyidxs: SmallVec<[TyIdx; 4]>,
    pub has_varargs: bool,
}

impl FuncTy {
    pub(super) fn new(
        rtn_tyidx: TyIdx,
        arg_tyidxs: SmallVec<[TyIdx; 4]>,
        has_varargs: bool,
    ) -> Self {
        Self {
            rtn_tyidx,
            arg_tyidxs,
            has_varargs,
        }
    }
}

#[derive(Debug)]
pub(super) struct TyIntExtra {
    value: u64,
}

index_vec::define_index_type! {
    pub(super) struct GuardIdx = u32;
}

index_vec::define_index_type! {
    pub(super) struct InstIdx = u32;
}

index_vec::define_index_type! {
    pub(super) struct TyIdx = u16;
}

index_vec::define_index_type! {
    pub(super) struct FuncDeclIdx = u16;
}

#[derive(Debug)]
pub(super) struct FuncDecl {
    pub name: String,
    pub fty: FuncTy,
}

impl FuncDecl {
    pub(super) fn new(name: String, ty: FuncTy) -> Self {
        Self { name, fty: ty }
    }

    fn rtn_tyidx(&self) -> TyIdx {
        self.fty.rtn_tyidx
    }
}

enum_dispatch!(
    pub(super) trait InstTrait {
        fn tyidx(&self, m: &Mod) -> TyIdx;
    }

    #[derive(Debug)]
    pub(super) enum Inst {
        Add(Add),
        Call(Call),
        Const(Const),
        Exit(Exit),
        Guard(Guard),
        ICmp(ICmp),
        LoadArg(LoadArg),
        Load(Load),
        Store(Store),
    }
);

/// `+` with normal LLVM semantics.
#[derive(Debug)]
pub(super) struct Add {
    pub tyidx: TyIdx,
    /// What LLVM calls `op1`.
    pub lhs: InstIdx,
    /// What LLVM calls `op2`.
    pub rhs: InstIdx,
    pub nuw: bool,
    pub nsw: bool,
}

impl Add {
    pub(super) fn new(tyidx: TyIdx, lhs: InstIdx, rhs: InstIdx, nuw: bool, nsw: bool) -> Self {
        Self {
            tyidx,
            lhs,
            rhs,
            nuw,
            nsw,
        }
    }
}

impl InstTrait for Add {
    fn tyidx(&self, _: &Mod) -> TyIdx {
        self.tyidx
    }
}

/// `call` of a known function with the semantics of LLVM calls where the follow LLVM
/// attributes are implicitly set/unset:
///   1.`tail` and `musttail` are false (i.e. not a tail call),
///   2. `fast-math` is false,
///   3. `cconv` is false,
///   4. `zeroext`, `signext`, `noext`, and `inreg` are false,
///   5. addrspace is 0,
///   6. no function attributes,
///   7. no operand bundles.
#[derive(Debug)]
pub(super) struct Call {
    pub tgt: InstIdx,
    pub fdclidx: FuncDeclIdx,
    pub args: SmallVec<[InstIdx; 1]>,
}

impl InstTrait for Call {
    fn tyidx(&self, _m: &Mod) -> TyIdx {
        todo!();
    }
}

impl Call {
    pub(super) fn new(tgt: InstIdx, fdidx: FuncDeclIdx, args: SmallVec<[InstIdx; 1]>) -> Self {
        Self {
            tgt,
            fdclidx: fdidx,
            args,
        }
    }
}

#[derive(Debug)]
pub(super) struct Const {
    pub tyidx: TyIdx,
    pub kind: ConstKind,
}

impl Const {
    pub(super) fn new(tyidx: TyIdx, kind: ConstKind) -> Self {
        Self { tyidx, kind }
    }
}

impl InstTrait for Const {
    fn tyidx(&self, _: &Mod) -> TyIdx {
        self.tyidx
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ConstKind {
    Int(ArbBitInt),
    Ptr(usize),
}

/// The block terminator: exactly how to interpret this depends on the kind of block.
#[derive(Debug)]
pub(super) struct Exit {
    pub iidxs: Vec<InstIdx>,
}

impl InstTrait for Exit {
    fn tyidx(&self, m: &Mod) -> TyIdx {
        todo!();
    }
}

/// A guard that the value produced by `cond` is `expect_true`. If not, the remainder of the
/// trace is invalid for this execution.
#[derive(Debug)]
pub(super) struct Guard {
    pub expect_true: bool,
    pub cond: InstIdx,
    pub vars: Vec<(aot_ir::InstId, InstIdx)>,
}

impl InstTrait for Guard {
    fn tyidx(&self, _m: &Mod) -> TyIdx {
        todo!();
    }
}

/// A comparison, with normal LLVM semantics.
#[derive(Debug)]
pub(super) struct ICmp {
    pub tyidx: TyIdx,
    /// What LLVM calls `cond`.
    pub kind: ICmpKind,
    /// What LLVM calls `op1`.
    pub lhs: InstIdx,
    /// What LLVM calls `op2`.
    pub rhs: InstIdx,
    pub samesign: bool,
}

impl InstTrait for ICmp {
    fn tyidx(&self, _: &Mod) -> TyIdx {
        self.tyidx
    }
}

#[derive(Debug)]
pub(super) enum ICmpKind {
    Eq,
    Ne,
    Ugt,
    Uge,
    Ult,
    Ule,
    Sgt,
    Sge,
    Slt,
    Sle,
}

#[derive(Debug)]
pub(super) struct Load {
    pub tyidx: TyIdx,
    pub ptr: InstIdx,
    pub is_volatile: bool,
}

impl Load {
    pub(super) fn new(tyidx: TyIdx, ptr: InstIdx, is_volatile: bool) -> Self {
        Self {
            tyidx,
            ptr,
            is_volatile,
        }
    }
}

impl InstTrait for Load {
    fn tyidx(&self, _: &Mod) -> TyIdx {
        self.tyidx
    }
}

/// Load argument `n`.
#[derive(Debug)]
pub(super) struct LoadArg {
    pub tyidx: TyIdx,
    pub n: u32,
}

impl LoadArg {
    pub(super) fn new(tyidx: TyIdx, n: u32) -> Self {
        Self { tyidx, n }
    }
}

impl InstTrait for LoadArg {
    fn tyidx(&self, _: &Mod) -> TyIdx {
        self.tyidx
    }
}

#[derive(Debug)]
pub(super) struct Store {
    pub ptr: InstIdx,
    pub val: InstIdx,
    pub is_volatile: bool,
}

impl Store {
    pub(super) fn new(ptr: InstIdx, val: InstIdx, is_volatile: bool) -> Self {
        Self {
            ptr,
            val,
            is_volatile,
        }
    }
}

impl InstTrait for Store {
    fn tyidx(&self, _: &Mod) -> TyIdx {
        todo!();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn inst_size() {
        assert_eq!(std::mem::size_of::<Inst>(), 48);
    }
}
