//! swarm-term: behavior and branding of the Swarm app built on Warp's engine.
//!
//! Swarm uses Warp's terminal model, blocks, input editor and renderer, but is
//! its own product: its own name, first-run flow and session UI (see
//! `docs/DESIGN.md` §9b in the swarm-term repo). Fork-specific switches live
//! here so the diff against upstream stays easy to find.

/// Swarm opens straight into a terminal: no Warp welcome, onboarding or login
/// screen. Warp's cloud sign-in stays available but opt-in.
pub const SKIP_WARP_ONBOARDING: bool = true;
