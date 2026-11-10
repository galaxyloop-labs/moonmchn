//! The name a flow claims, which a guest chooses: the input is the opening bytes it sends. The
//! decider reads them as a TLS `ClientHello`, takes its SNI, and normalizes that before any
//! lookup. A panic, a hang or a runaway allocation here is the guest's to trigger.

#![no_main]

use libfuzzer_sys::fuzz_target;
use moonmchn::config::Subject;
use moonmchn::daemon::fuzzing::claimed_name;

fuzz_target!(|opening: &[u8]| {
    if let Some(claimed) = claimed_name(opening) {
        // The name `bind_name` would look up, or none. Only reaching it matters: a panic or a
        // hang on the way fails the run.
        drop(Subject::normalize_name(&claimed));
    }
});
