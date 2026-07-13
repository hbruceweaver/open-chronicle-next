//! Narrow, versioned C ABI for the signed macOS application.
//!
//! Exported functions and explicit buffer ownership begin in U6. Release builds
//! retain unwinding so every future ABI entry point can contain Rust panics.
