//! Process-wide configuration.
//!
//! Holds the global **default timeout** used wherever an operation waits but
//! the caller did not pass an explicit timeout: `Locator` auto-wait before
//! action methods, and the language bindings' `wait_*` / app-lookup defaults.
//!
//! Resolution order (first match wins):
//! 1. An explicit per-call timeout (`Locator::with_timeout`, a `timeout=`
//!    argument in the bindings).
//! 2. The programmatic global set via [`set_default_timeout`].
//! 3. The `XA11Y_DEFAULT_TIMEOUT` environment variable (seconds, e.g. `30`
//!    or `2.5`), read and parsed once on first use.
//! 4. The built-in default of 5 seconds.
//!
//! An invalid `XA11Y_DEFAULT_TIMEOUT` value is an error
//! ([`Error::InvalidConfig`]), not a silent fall-through to the built-in
//! default — see the "no silent fallbacks" design tenet.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::error::{Error, Result};

/// Environment variable holding the default timeout in seconds.
///
/// Read once, on the first call to [`default_timeout`] — later changes to the
/// process environment have no effect.
pub const DEFAULT_TIMEOUT_ENV_VAR: &str = "XA11Y_DEFAULT_TIMEOUT";

/// Built-in default used when neither [`set_default_timeout`] nor
/// [`DEFAULT_TIMEOUT_ENV_VAR`] provides a value.
const BUILTIN_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Programmatic override set by [`set_default_timeout`]. Takes precedence
/// over the environment variable.
static PROGRAMMATIC_DEFAULT: Mutex<Option<Duration>> = Mutex::new(None);

/// Memoized result of reading [`DEFAULT_TIMEOUT_ENV_VAR`]:
/// `Ok(None)` = unset, `Ok(Some(_))` = parsed value, `Err(_)` = present but
/// invalid (the message is replayed on every `default_timeout()` call).
static ENV_DEFAULT: OnceLock<std::result::Result<Option<Duration>, String>> = OnceLock::new();

/// Parse a timeout value in seconds (`"30"`, `"2.5"`, `"0"`).
fn parse_timeout_secs(raw: &str) -> std::result::Result<Duration, String> {
    let secs: f64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("expected a number of seconds, got {raw:?}"))?;
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!(
            "expected a finite, non-negative number of seconds, got {raw:?}"
        ));
    }
    Ok(Duration::from_secs_f64(secs))
}

fn env_default() -> std::result::Result<Option<Duration>, String> {
    ENV_DEFAULT
        .get_or_init(|| match std::env::var(DEFAULT_TIMEOUT_ENV_VAR) {
            Ok(raw) => parse_timeout_secs(&raw)
                .map(Some)
                .map_err(|msg| format!("{DEFAULT_TIMEOUT_ENV_VAR}: {msg}")),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!(
                "{DEFAULT_TIMEOUT_ENV_VAR}: value is not valid Unicode"
            )),
        })
        .clone()
}

/// Set the process-wide default timeout.
///
/// Becomes the timeout for every auto-wait and `wait_*` operation that does
/// not pass an explicit timeout. Takes effect for *all* subsequent
/// operations, including on `Locator`s created before this call (the default
/// is resolved at use time, not construction time). Takes precedence over
/// [`DEFAULT_TIMEOUT_ENV_VAR`].
///
/// `Duration::ZERO` keeps the "single attempt, no polling" semantics.
pub fn set_default_timeout(timeout: Duration) {
    *PROGRAMMATIC_DEFAULT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(timeout);
}

/// Get the effective process-wide default timeout.
///
/// See the [module docs](self) for the resolution order.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] if [`DEFAULT_TIMEOUT_ENV_VAR`] is set to
/// a value that is not a finite, non-negative number of seconds (and no
/// programmatic default has been set to take precedence over it).
pub fn default_timeout() -> Result<Duration> {
    if let Some(t) = *PROGRAMMATIC_DEFAULT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
    {
        return Ok(t);
    }
    match env_default() {
        Ok(Some(t)) => Ok(t),
        Ok(None) => Ok(BUILTIN_DEFAULT_TIMEOUT),
        Err(message) => Err(Error::InvalidConfig { message }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::locator::Locator;
    use crate::mock::build_provider;
    use crate::provider::Provider;

    #[test]
    fn parse_accepts_integers_floats_and_zero() {
        assert_eq!(parse_timeout_secs("30").unwrap(), Duration::from_secs(30));
        assert_eq!(
            parse_timeout_secs("2.5").unwrap(),
            Duration::from_secs_f64(2.5)
        );
        assert_eq!(parse_timeout_secs("0").unwrap(), Duration::ZERO);
        assert_eq!(
            parse_timeout_secs(" 10 ").unwrap(),
            Duration::from_secs(10),
            "surrounding whitespace must be tolerated"
        );
    }

    #[test]
    fn parse_rejects_non_numeric_negative_and_non_finite() {
        for bad in ["abc", "", "-1", "inf", "NaN", "5s", "1,5"] {
            let err = parse_timeout_secs(bad)
                .expect_err(&format!("{bad:?} must be rejected as a timeout"));
            assert!(
                err.contains("seconds"),
                "error must explain the expected unit: {err}"
            );
        }
    }

    /// All assertions about the *global* default live in this single test so
    /// they run sequentially — `set_default_timeout` mutates process-wide
    /// state and parallel test threads would otherwise race on it.
    #[test]
    fn global_default_precedence_and_locator_integration() {
        // Built-in default applies when nothing is configured. Skip the
        // assertion if the developer's environment sets the env var — that's
        // the documented behavior, not a failure.
        if std::env::var_os(DEFAULT_TIMEOUT_ENV_VAR).is_none() {
            assert_eq!(default_timeout().unwrap(), BUILTIN_DEFAULT_TIMEOUT);
        }

        // A locator created *before* set_default_timeout must still honor
        // it: the default is resolved at use time.
        let provider: Arc<dyn Provider> = build_provider();
        let missing = Locator::new(provider.clone(), None, r#"button[name="DoesNotExist"]"#);

        set_default_timeout(Duration::from_millis(150));
        assert_eq!(default_timeout().unwrap(), Duration::from_millis(150));

        let start = std::time::Instant::now();
        let err = missing
            .press()
            .expect_err("press on a never-matching selector must time out");
        assert!(matches!(err, Error::Timeout { .. }), "got {err:?}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "auto-wait must use the 150ms global default, not the 5s built-in; took {:?}",
            start.elapsed()
        );

        // An explicit with_timeout wins over the global default.
        set_default_timeout(Duration::from_secs(30));
        let start = std::time::Instant::now();
        let err = missing
            .clone()
            .with_timeout(Duration::from_millis(150))
            .press()
            .expect_err("press on a never-matching selector must time out");
        assert!(matches!(err, Error::Timeout { .. }), "got {err:?}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "explicit with_timeout must beat the 30s global default; took {:?}",
            start.elapsed()
        );

        // Restore the built-in value so other tests in this process see the
        // documented default.
        set_default_timeout(BUILTIN_DEFAULT_TIMEOUT);
    }
}
