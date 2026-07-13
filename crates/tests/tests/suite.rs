//! The shared behavioral suite, instantiated per engine.
//!
//! When the fast engine lands it gets one more line here and must pass
//! the exact same tests as the oracle.

tessera_tests::engine_suite!(reference, tessera_reference::ReferenceBook);
