//! The J2 trace compiler.
//!
//! This is a "reverse code generation" trace compiler. At a high-level it has three main passes:
//!
//! 1. Build a HIR trace from a sequence of AOT blocks ([aot_to_hir]).
//! 2. Optimise a HIR trace (not yet implemented).
//! 3. Assemble a HIR trace to machine code (using [asm_guide], [regalloc], and an
//!    architecture-dependent backend).
//!
//! Passes 1 and 2 are "forward" (i.e. normal) passes. Pass 3 is a "reverse" pass: roughly
//! speaking, it iterates from the last to the first instruction in a trace.

#![allow(unused)]

mod aot_to_hir;
mod hir;
mod hir_to_asm;
mod regalloc;
#[cfg(target_arch = "x86_64")]
mod x64;

use crate::compile::{jitc_yk::AOT_MOD, CompilationError, CompiledTrace, Compiler};
use std::{error::Error, sync::Arc};

#[derive(Debug)]
pub(super) struct J1;

impl J1 {
    pub(super) fn new() -> Result<Arc<Self>, Box<dyn Error>> {
        Ok(Arc::new(Self))
    }
}

impl Compiler for J1 {
    fn root_compile(
        &self,
        mt: std::sync::Arc<crate::MT>,
        ta_iter: Box<dyn crate::trace::AOTTraceIterator>,
        trid: crate::mt::TraceId,
        hl: std::sync::Arc<parking_lot::Mutex<crate::location::HotLocation>>,
        promotions: Box<[u8]>,
        debug_strs: Vec<String>,
        coupler: Option<std::sync::Arc<dyn CompiledTrace>>,
    ) -> Result<Arc<dyn CompiledTrace>, CompilationError> {
        let hm = aot_to_hir::AotToHir::new(
            &mt,
            &AOT_MOD,
            ta_iter,
            trid,
            aot_to_hir::BuildKind::Loop,
            promotions,
            debug_strs,
            coupler,
        )
        .build()?;

        #[cfg(target_arch = "x86_64")]
        let be = x64::x64hir_to_asm::X64HirToAsm::new(&hm);

        hir_to_asm::HirToAsm::new(&hm, be).build()
    }

    fn sidetrace_compile(
        &self,
        _mt: std::sync::Arc<crate::MT>,
        _aottrace_iter: Box<dyn crate::trace::AOTTraceIterator>,
        _ctrid: crate::mt::TraceId,
        _parent_ctr: std::sync::Arc<dyn CompiledTrace>,
        _gidx: super::GuardIdx,
        _target_ctr: std::sync::Arc<dyn CompiledTrace>,
        _hl: std::sync::Arc<parking_lot::Mutex<crate::location::HotLocation>>,
        _promotions: Box<[u8]>,
        _debug_strs: Vec<String>,
    ) -> Result<std::sync::Arc<dyn CompiledTrace>, CompilationError> {
        todo!()
    }
}
