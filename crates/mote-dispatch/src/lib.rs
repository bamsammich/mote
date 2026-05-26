//! Differentiated hook dispatch for Mote.
//!
//! The hook-type-differentiated dispatch engine: **filter chains** (10ms sync,
//! hard timeout to `defer`, first-block-wins, modify-cascades, priority
//! ordering), **broadcasts** (100ms async-allowed, no return semantics, error
//! isolation), **keybind handlers** (input-coalescing, no raw-timeout
//! auto-disable), and the **collector** pattern used inside exclusive
//! capabilities. Enforces three-errors-in-24h auto-disable (excluding
//! keybinds) and routes capability API invocations to the current fulfiller,
//! executing under the fulfiller's permissions. Registration requires the hook
//! type so the runtime enforces the matching contract.
//!
//! This crate is a stub awaiting the `1D.1` implementation wave.
