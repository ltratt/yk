use crate::compile::j2::regalloc::RegT;
use iced_x86::Register;
use strum::EnumCount;

#[derive(Clone, Copy, Debug, EnumCount, PartialEq)]
// If the `repr` changes from `u8`, the `as` in the `Reg::regidx()` function will also need
// updating.
#[repr(u8)]
pub(in crate::compile::j2) enum Reg {
    // The general purpose registers relevant to our allocator. RSP and RBP are reserved and thus
    // not listed here. The values we assign here are irrelevant semantically, though if they're
    // not consecutive, the register allocator will necessarily waste space.
    RAX = 0,
    RCX,
    RDX,
    RBX,
    RSI,
    RDI,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,

    // Floating point registers. All of these are available to us.
    XMM0,
    XMM1,
    XMM2,
    XMM3,
    XMM4,
    XMM5,
    XMM6,
    XMM7,
    XMM8,
    XMM9,
    XMM10,
    XMM11,
    XMM12,
    XMM13,
    XMM14,
    XMM15,
}

impl Reg {
    pub(super) fn bitw(self) -> u32 {
        match self {
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
            | Reg::R15 => 64,
            Reg::XMM0
            | Reg::XMM1
            | Reg::XMM2
            | Reg::XMM3
            | Reg::XMM4
            | Reg::XMM5
            | Reg::XMM6
            | Reg::XMM7
            | Reg::XMM8
            | Reg::XMM9
            | Reg::XMM10
            | Reg::XMM11
            | Reg::XMM12
            | Reg::XMM13
            | Reg::XMM14
            | Reg::XMM15 => 128,
        }
    }

    pub(super) fn to_reg8(self) -> Register {
        match self {
            Reg::RAX => Register::AL,
            Reg::RCX => Register::CL,
            Reg::RDX => Register::DL,
            Reg::RBX => Register::BL,
            Reg::RSI => Register::SIL,
            Reg::RDI => Register::DIL,
            Reg::R8 => Register::R8L,
            Reg::R9 => Register::R9L,
            Reg::R10 => Register::R10L,
            Reg::R11 => Register::R11L,
            Reg::R12 => Register::R12L,
            Reg::R13 => Register::R13L,
            Reg::R14 => Register::R14L,
            Reg::R15 => Register::R15L,
            _ => unreachable!(),
        }
    }

    pub(super) fn to_reg32(self) -> Register {
        match self {
            Reg::RAX => Register::EAX,
            Reg::RCX => Register::ECX,
            Reg::RDX => Register::EDX,
            Reg::RBX => Register::EBX,
            Reg::RSI => Register::ESI,
            Reg::RDI => Register::EDI,
            Reg::R8 => Register::R8D,
            Reg::R9 => Register::R9D,
            Reg::R10 => Register::R10D,
            Reg::R11 => Register::R11D,
            Reg::R12 => Register::R12D,
            Reg::R13 => Register::R13D,
            Reg::R14 => Register::R14D,
            Reg::R15 => Register::R15D,
            _ => unreachable!(),
        }
    }

    pub(super) fn to_reg64(self) -> Register {
        match self {
            Reg::RAX => Register::RAX,
            Reg::RCX => Register::RCX,
            Reg::RDX => Register::RDX,
            Reg::RBX => Register::RBX,
            Reg::RSI => Register::RSI,
            Reg::RDI => Register::RDI,
            Reg::R8 => Register::R8,
            Reg::R9 => Register::R9,
            Reg::R10 => Register::R10,
            Reg::R11 => Register::R11,
            Reg::R12 => Register::R12,
            Reg::R13 => Register::R13,
            Reg::R14 => Register::R14,
            Reg::R15 => Register::R15,
            _ => unreachable!(),
        }
    }
}

impl RegT for Reg {
    type RegIdx = RegIdx;

    fn max_regidx() -> RegIdx {
        RegIdx::from(Reg::COUNT)
    }

    fn regidx(&self) -> Self::RegIdx {
        RegIdx::from(*self as u8)
    }
}

index_vec::define_index_type! {
    pub(in crate::compile::j2) struct RegIdx = u8;
    IMPL_RAW_CONVERSIONS = true;
}

pub(super) const NORMAL_GP_REGS: [Reg; 14] = [
    Reg::RAX,
    Reg::RCX,
    Reg::RDX,
    Reg::RBX,
    Reg::RSI,
    Reg::RDI,
    Reg::R8,
    Reg::R9,
    Reg::R10,
    Reg::R11,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::R15,
];

pub(super) const ALL_XMM_REGS: [Reg; 16] = [
    Reg::XMM0,
    Reg::XMM1,
    Reg::XMM2,
    Reg::XMM3,
    Reg::XMM4,
    Reg::XMM5,
    Reg::XMM6,
    Reg::XMM7,
    Reg::XMM8,
    Reg::XMM9,
    Reg::XMM10,
    Reg::XMM11,
    Reg::XMM12,
    Reg::XMM13,
    Reg::XMM14,
    Reg::XMM15,
];
