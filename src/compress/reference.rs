//! oozextract, a pure-Rust Oodle decoder, behind one boundary: its refusals and its panics (its decoder asserts in some
//! places where it should refuse) both come back as a [`Failure`], so a corrupt or unsupported stream is an error and
//! never a panic. Off Windows it is what [`Oodle::open`](crate::compress::oodle::Oodle::open) decodes with when given
//! no library path; on Windows it is there only with the `reference-decoder` feature, as a second decoder to compare
//! the game's own library against.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// Why oozextract did not give the expected bytes.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
    /// oozextract returned an error for the stream.
    #[error("oozextract refused the stream: {0}")]
    Refused(oozextract::OozError),
    /// oozextract panicked instead of returning an error (a defect in oozextract); the panic message.
    #[error("oozextract panicked instead of refusing the stream (an upstream defect): {0}")]
    Panicked(String),
    /// The output buffer for the expected size could not be allocated.
    #[error("cannot allocate the expected {expected} output bytes")]
    Limit {
        /// The bytes asked for.
        expected: usize,
    },
    /// oozextract decoded without error but produced a different number of bytes.
    #[error("oozextract wrote {written} bytes, expected {expected}")]
    Short {
        /// The bytes it wrote.
        written: usize,
        /// The bytes asked for.
        expected: usize,
    },
}

impl Failure {
    /// Whether this is a refusal for `reason`: oozextract wraps its error once per layer it passes through, each wrapper
    /// without a message of its own, so the reason is that of the innermost error that states one.
    pub fn refuses_for(&self, reason: &str) -> bool {
        let Failure::Refused(e) = self else {
            return false;
        };
        let mut at: Option<&(dyn std::error::Error + 'static)> = Some(e);
        while let Some(error) = at {
            if error
                .downcast_ref::<oozextract::OozError>()
                .is_some_and(|ooz| ooz.message.as_deref() == Some(reason))
            {
                return true;
            }
            at = error.source();
        }
        false
    }
}

/// The `expected` bytes oozextract decodes from `input`, or why not.
pub fn decode(input: &[u8], expected: usize) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    out.try_reserve_exact(expected)
        .map_err(|_| Failure::Limit { expected })?;
    out.resize(expected, 0);
    let written = catch_unwind(AssertUnwindSafe(|| {
        oozextract::Extractor::new().read_from_slice(input, &mut out)
    }))
    .map_err(|payload| {
        Failure::Panicked(
            payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a panic without a message".to_owned()),
        )
    })?
    .map_err(Failure::Refused)?;
    if written != expected {
        return Err(Failure::Short { written, expected });
    }
    Ok(out)
}
