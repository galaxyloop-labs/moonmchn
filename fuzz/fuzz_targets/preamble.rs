//! The preamble an engine's gateway writes before each flow, which the decider reads from any
//! local process that connects, before it checks the token. The input is the connection's
//! stream: the decider reads exactly as many bytes as each `Decoded::Partial` asks for.
//!
//! Checks the contract `Gateway::decode_flow` documents, which the decider's reads rely on:
//! - every `Partial` asks for more bytes than it holds;
//! - a `Complete` request needs every byte read, so the decider read nothing past it. A decoder
//!   that asks for more would hold an engine's flow until the deadline denies it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use moonmchn::machine::{Decoded, Gateway as _};
use moonmchn_smolmachines::Gateway;

fuzz_target!(|stream: &[u8]| {
    let mut read = 0;
    while let Some(bytes) = stream.get(..read) {
        match Gateway::decode_flow(bytes) {
            Ok(Decoded::Partial(wanted)) => {
                assert!(
                    wanted > read,
                    "Partial({wanted}) with {read} bytes read asks for none"
                );
                read = wanted;
            }
            Ok(Decoded::Complete(_)) => {
                let shorter = bytes.split_last().map(|(_, shorter)| shorter);
                if let Some(shorter) = shorter {
                    assert!(
                        !matches!(Gateway::decode_flow(shorter), Ok(Decoded::Complete(_))),
                        "a request {read} bytes long decodes from {} bytes",
                        shorter.len()
                    );
                }
                return;
            }
            Err(_) => return,
        }
    }
    // The stream ended before the request did, which the decider reads as a short request.
});
