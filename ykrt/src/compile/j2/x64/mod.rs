use crate::{
    compile::{j2::hir::*, jitc_yk::aot_ir, CompilationError, CompiledTrace, Guard, GuardIdx},
    location::HotLocation,
    mt::TraceId,
};
use std::{error::Error, ffi::c_void, sync::Arc};

mod asm;
pub(super) mod x64hir_to_asm;
mod x64regalloc;

#[derive(Debug)]
struct X64CompiledTrace {
    trid: TraceId,
    exe: SyncSafePtr<*mut c_void>,
}

impl X64CompiledTrace {
    fn new(trid: TraceId, exe: *mut c_void) -> Self {
        Self {
            trid,
            exe: SyncSafePtr(exe),
        }
    }
}

impl CompiledTrace for X64CompiledTrace {
    fn ctrid(&self) -> TraceId {
        self.trid
    }

    fn safepoint(&self) -> &Option<aot_ir::DeoptSafepoint> {
        todo!()
    }

    fn as_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync + 'static> {
        todo!()
    }

    fn guard(&self, gidx: GuardIdx) -> &Guard {
        todo!()
    }

    fn patch_guard(&self, gidx: GuardIdx, target: *const std::ffi::c_void) {
        todo!()
    }

    fn entry(&self) -> *const std::ffi::c_void {
        self.exe.0
    }

    fn entry_sp_off(&self) -> usize {
        todo!()
    }

    fn hl(&self) -> &std::sync::Weak<parking_lot::Mutex<HotLocation>> {
        todo!()
    }

    fn disassemble(&self, with_addrs: bool) -> Result<String, Box<dyn Error>> {
        todo!()
    }
}

#[derive(Debug)]
struct SyncSafePtr<T>(T);
unsafe impl<T> Send for SyncSafePtr<T> {}
unsafe impl<T> Sync for SyncSafePtr<T> {}
