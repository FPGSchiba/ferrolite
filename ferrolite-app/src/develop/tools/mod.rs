//! Concrete Develop tools registered by `DevelopToolRegistry::standard()`. Each wraps
//! the existing overlay/panel functions so the migration is behavior-preserving.

#![allow(dead_code)] // consumed by standard() in Task 9; allow removed at Task 13

pub mod crop;
pub mod mask;
