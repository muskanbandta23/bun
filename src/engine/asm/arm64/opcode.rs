//! Families of instructions that share a form.

use crate::operand::Cond;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cc {
    Eq,
    Ne,
    Hs,
    Lo,
    Mi,
    Pl,
    Vs,
    Vc,
    Hi,
    Ls,
    Ge,
    Lt,
    Gt,
    Le,
}

impl Cc {
    pub const ALL: [Self; 14] = [
        Self::Eq,
        Self::Ne,
        Self::Hs,
        Self::Lo,
        Self::Mi,
        Self::Pl,
        Self::Vs,
        Self::Vc,
        Self::Hi,
        Self::Ls,
        Self::Ge,
        Self::Lt,
        Self::Gt,
        Self::Le,
    ];

    #[inline]
    pub const fn inverted(self) -> Self {
        Self::ALL[self as usize ^ 1]
    }
}

impl From<Cond> for Cc {
    #[inline]
    fn from(cond: Cond) -> Self {
        match cond {
            Cond::Eq => Self::Eq,
            Cond::Ne => Self::Ne,
            Cond::Lt => Self::Lt,
            Cond::Ge => Self::Ge,
            Cond::Le => Self::Le,
            Cond::Gt => Self::Gt,
            Cond::Below => Self::Lo,
            Cond::AboveEq => Self::Hs,
            Cond::BelowEq => Self::Ls,
            Cond::Above => Self::Hi,
        }
    }
}
