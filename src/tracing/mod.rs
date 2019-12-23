#![allow(unused_variables)]

//! The tracing module provides hook points for runtime level
//! tracing. It is always available, but its functions will be
//! inlined by the compiler and never be called.
//! 
//! Activating the `tracing` feature will enable these functions
//! and allow you to trace calls to them with tools like `perf` or
//! `dtrace`.

pub mod task;
pub mod rt;
