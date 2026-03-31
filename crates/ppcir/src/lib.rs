//! Stack-machine IR for PowerPC, inspired by Play!'s Jitter framework.
//!
//! `ppcir` provides a typed, stack-machine intermediate representation that
//! sits between PowerPC instruction decoding and any backend code generator
//! (WebAssembly via `ppcwasm`, or native via Cranelift in `ppcjit`).
//!
//! The key design insight — taken from the Play! PS2 emulator's Jitter library
//! — is that an IR layer lets you **decode PowerPC once** and **lower to
//! multiple targets**, while also serving as the right place to add simple
//! optimisations (constant folding, dead-store elimination).

pub mod decode;
pub mod inst;

pub use decode::Decoder;
pub use inst::{IrBlock, IrInst, IrLocal, IrTy};
