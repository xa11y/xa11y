//! One deadline for a whole operation, nested provider reads included.
//!
//! A locator wait used to bound only its own poll loop. Each provider read
//! inside it carried the backend's fixed native timeout (5 s for an AT-SPI
//! method, 2 s for an AX message, 20 s for a UIA transaction), so a 1 s
//! `wait_visible` against a hung app could run for many seconds past the
//! caller's timeout (issue #531).
//!
//! Core now opens a [`Deadline`] for the length of every operation that takes
//! a timeout (auto-waiting actions, `wait_*`, application lookup) with
//! [`Deadline::scope`]. While the scope is open, [`Deadline::current`] returns
//! it on that thread, and a backend caps each native timeout at the time left
//! with [`call_timeout`]. The caller's timeout is then an upper bound for the
//! whole operation.
//!
//! # Why it is ambient
//!
//! The deadline is carried by the thread rather than added as a parameter to
//! every [`Provider`](crate::Provider) method. A parameter would stop at the
//! trait boundary: the native call that needs the bound sits several private
//! functions deeper in each backend, and every one of them would need the
//! same parameter. A read added later would compile without it and quietly
//! fall back to the fixed timeout. Read from the thread, every native call
//! that goes through [`call_timeout`] is bounded, however deep it sits.
//!
//! The cost is that a backend handing work to another thread (a rayon pool,
//! a COM worker) must carry the deadline across itself: capture
//! [`Deadline::current`] before the hop and re-open it with
//! [`Deadline::scope_opt`] on the other side.
//!
//! # For provider implementers
//!
//! A custom provider keeps compiling. To honour the deadline, it gets each
//! native call's timeout from [`call_timeout`], and when that call times out
//! with [`CallTimeout::is_deadline`] set, it returns
//! [`CallTimeout::expired`] naming the call and its target. That error's
//! [`Diagnosis::pending`] is what the caller sees as "still pending".
//!
//! # What does not change
//!
//! A zero timeout still means "one attempt, no polling". That attempt runs
//! with no deadline, so its reads keep the backend's own bound: a deadline
//! that is already over would fail every read before it was made.

use std::cell::Cell;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{Diagnosis, Error};

thread_local! {
    /// The innermost open [`Deadline::scope`] on this thread.
    static CURRENT: Cell<Option<Deadline>> = const { Cell::new(None) };
}

/// The point in time an operation must finish by, and the budget it was
/// given.
///
/// See the [module docs](self) for how it reaches the backends.
#[derive(Debug, Clone)]
pub struct Deadline {
    at: Instant,
    budget: Duration,
    /// The first call this deadline cut short, shared by every thread the
    /// deadline was carried to. A backend may map that call's failure into
    /// whatever error its call site builds; this is how core still learns
    /// which call it was.
    cut: Arc<OnceLock<String>>,
}

impl Deadline {
    /// A deadline `budget` from now.
    pub fn after(budget: Duration) -> Self {
        let now = Instant::now();
        Self {
            // An `Instant` this far out cannot be represented on some
            // platforms; a budget that large is "no deadline" in practice.
            at: now
                .checked_add(budget)
                .unwrap_or(now + Duration::from_secs(86_400 * 365)),
            budget,
            cut: Arc::default(),
        }
    }

    /// The instant the operation must finish by.
    pub fn at(&self) -> Instant {
        self.at
    }

    /// The timeout the operation was given, as the caller wrote it.
    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// Time left before the deadline, zero once it has passed.
    pub fn remaining(&self) -> Duration {
        self.at.saturating_duration_since(Instant::now())
    }

    /// Whether the deadline has passed.
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.at
    }

    /// The deadline of the innermost open [`scope`](Self::scope) on this
    /// thread, if any.
    pub fn current() -> Option<Deadline> {
        CURRENT.with(|c| {
            let current = c.take();
            c.set(current.clone());
            current
        })
    }

    /// The call this deadline cut short, once a backend has reported one
    /// through [`expired`](Self::expired), from any thread it was carried to.
    pub fn cut_short(&self) -> Option<&str> {
        self.cut.get().map(String::as_str)
    }

    /// Run `f` with this deadline open on the current thread.
    ///
    /// A scope nested inside another keeps whichever deadline is earlier, so
    /// an inner wait can only shorten the operation around it, never extend
    /// it. The previous deadline is restored when `f` returns or unwinds.
    pub fn scope<R>(self, f: impl FnOnce() -> R) -> R {
        let effective = match Self::current() {
            Some(outer) if outer.at <= self.at => outer,
            _ => self,
        };
        let _restore = Restore(CURRENT.with(|c| c.replace(Some(effective))));
        f()
    }

    /// Run `f` with this deadline in place of the current one, even when the
    /// current one is nearer.
    ///
    /// This is for collecting a failure's diagnosis once the operation's own
    /// deadline has passed: under it, every read the diagnosis needs would
    /// be refused before it was made. Use a short budget. The previous
    /// deadline is restored when `f` returns or unwinds.
    pub fn scope_instead<R>(self, f: impl FnOnce() -> R) -> R {
        let _restore = Restore(CURRENT.with(|c| c.replace(Some(self))));
        f()
    }

    /// [`scope`](Self::scope) when `deadline` is `Some`, otherwise just `f`.
    ///
    /// This is the form for carrying a deadline across a thread hop: capture
    /// [`current`](Self::current) before handing work to another thread and
    /// open it again there.
    pub fn scope_opt<R>(deadline: Option<Deadline>, f: impl FnOnce() -> R) -> R {
        match deadline {
            Some(d) => d.scope(f),
            None => f(),
        }
    }

    /// The error for an operation whose deadline passed while `pending` was
    /// still in flight (or, with `None`, between calls).
    ///
    /// `pending` names the call and what it was addressed to; it lands in
    /// [`Diagnosis::pending`], and is recorded as the call this deadline
    /// [cut short](Self::cut_short) (the first one reported wins).
    pub fn expired(&self, pending: Option<String>) -> Error {
        if let Some(p) = &pending {
            // Already set means an earlier call was cut first; that one is
            // what the operation hung in, so it stays.
            let _ = self.cut.set(p.clone());
        }
        let mut d = Diagnosis::new().condition(format!(
            "operation to finish within its {:?} timeout",
            self.budget
        ));
        d.pending = pending;
        Error::timeout(self.budget).diagnose(d)
    }
}

/// Two deadlines are equal when they end at the same instant with the same
/// budget; which call either one cut short is a record, not identity.
impl PartialEq for Deadline {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.budget == other.budget
    }
}

impl Eq for Deadline {}

/// Puts the outer deadline back when a scope ends, on unwind too.
struct Restore(Option<Deadline>);

impl Drop for Restore {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0.take()));
    }
}

/// The timeout one native call should use.
///
/// Built by [`call_timeout`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CallTimeout {
    /// The timeout to hand the platform API.
    pub duration: Duration,
    /// The open deadline, when it is what set [`duration`](Self::duration)
    /// (the time left was shorter than the backend's own bound).
    pub deadline: Option<Deadline>,
}

impl CallTimeout {
    /// Whether the caller's deadline, rather than the backend's own bound, is
    /// what this call will time out on. When it does, report it with
    /// [`expired`](Self::expired).
    pub fn is_deadline(&self) -> bool {
        self.deadline.is_some()
    }

    /// The deadline error for this call having timed out. `pending` names
    /// the call and its target. `None` when the backend's own bound applied:
    /// the backend reports that timeout in its own words.
    pub fn expired(&self, pending: impl Into<String>) -> Option<Error> {
        self.deadline
            .as_ref()
            .map(|d| d.expired(Some(pending.into())))
    }
}

/// Cap a native call's timeout at the time left on this thread's open
/// deadline.
///
/// `default` is the backend's own bound for the call. Returns it unchanged
/// when no deadline is open or the deadline is further away.
///
/// # Errors
///
/// The deadline's [`expired`](Deadline::expired) error, naming `pending`,
/// when it has already passed. The call must not be made: most platform APIs
/// read a zero timeout as "use the default" or "wait forever".
pub fn call_timeout(
    default: Duration,
    pending: impl FnOnce() -> String,
) -> crate::Result<CallTimeout> {
    match Deadline::current() {
        Some(d) => {
            let remaining = d.remaining();
            if remaining.is_zero() {
                Err(d.expired(Some(pending())))
            } else if remaining < default {
                Ok(CallTimeout {
                    duration: remaining,
                    deadline: Some(d),
                })
            } else {
                Ok(CallTimeout {
                    duration: default,
                    deadline: None,
                })
            }
        }
        None => Ok(CallTimeout {
            duration: default,
            deadline: None,
        }),
    }
}

/// Run `f` under a deadline of `timeout`, except that a zero timeout opens
/// none (see the [module docs](self#what-does-not-change)).
///
/// This is how core's own waits open their scope.
pub(crate) fn within<R>(timeout: Duration, f: impl FnOnce() -> R) -> R {
    if timeout.is_zero() {
        f()
    } else {
        Deadline::after(timeout).scope(f)
    }
}

/// Sleep one poll `interval`, cut short at `timeout` after `start`. False
/// when the timeout has run out, so the wait ends on the poll it just made
/// rather than starting one under an expired deadline.
pub(crate) fn pause(start: std::time::Instant, timeout: Duration, interval: Duration) -> bool {
    let left = timeout.saturating_sub(start.elapsed());
    if left.is_zero() {
        return false;
    }
    std::thread::sleep(interval.min(left));
    start.elapsed() < timeout
}

/// How long collecting a failure's diagnosis may take once the operation's
/// own deadline has passed.
///
/// A wait that times out still owes the caller its near-miss candidates and
/// a bounded scope snapshot (tenet 6), and those are provider reads too.
/// Under the expired deadline every one of them would fail before it was
/// made, so the failure path gets a budget of its own. The worst case for a
/// failing call is therefore its timeout plus this.
pub(crate) const DIAGNOSIS_BUDGET: Duration = Duration::from_secs(2);

/// How long a wait whose read the deadline cut may spend on its miss
/// context. The cut read may mean the application is hung, so this is kept
/// short enough that such a wait still ends near its own timeout.
pub(crate) const CUT_DIAGNOSIS_BUDGET: Duration = Duration::from_millis(250);

/// Run `f`, a cut wait's miss-context collection, under a fresh
/// [`CUT_DIAGNOSIS_BUDGET`] deadline that replaces the current one.
pub(crate) fn for_cut_diagnosis<R>(f: impl FnOnce() -> R) -> R {
    Deadline::after(CUT_DIAGNOSIS_BUDGET).scope_instead(f)
}

/// Run `f`, the collection of a failure's diagnosis, under a fresh
/// [`DIAGNOSIS_BUDGET`] deadline that replaces the current one.
pub(crate) fn for_diagnosis<R>(f: impl FnOnce() -> R) -> R {
    Deadline::after(DIAGNOSIS_BUDGET).scope_instead(f)
}

/// When `err` is a deadline expiry from a provider read, the call it names
/// as pending.
///
/// Core's waits use this to fold an expiry that happened inside a read into
/// their own timeout, so the error names the selector and condition as well
/// as the read that was cut short.
///
/// Any error counts once the deadline has recorded a call it
/// [cut short](Deadline::cut_short): the backend reported the expiry there,
/// and the error its call site then built (a platform error, say) is a
/// consequence of it.
pub(crate) fn pending_of(err: &Error) -> Option<Option<String>> {
    let deadline = Deadline::current().filter(Deadline::is_expired)?;
    match err {
        Error::Timeout { .. } => Some(
            err.diagnosis()
                .and_then(|d| d.pending.clone())
                .or_else(|| deadline.cut_short().map(str::to_owned))
                // A backend that has not adopted `call_timeout` reports its
                // own timeout; keep what it said.
                .or_else(|| Some(err.to_string())),
        ),
        _ => deadline.cut_short().map(|cut| Some(cut.to_owned())),
    }
}

/// `r`, unless this thread's deadline cut a read short while producing it.
///
/// Backends tolerate a missing field, so a snapshot whose role or state read
/// was cut can come back `Ok` with that field absent, and a children read
/// cut short can come back empty and read as "no match". Either would let a
/// wait conclude from data it never saw (`wait_hidden` succeeding on a state
/// that was not read), so once a cut is recorded the result is replaced by
/// the deadline's expiry naming it.
pub(crate) fn distrust<T>(r: crate::Result<T>) -> crate::Result<T> {
    let cut = Deadline::current()
        .filter(Deadline::is_expired)
        .and_then(|d| d.cut_short().map(|c| d.expired(Some(c.to_owned()))));
    match cut {
        Some(err) => Err(err),
        None => r,
    }
}

/// An action's platform error, as the deadline expiry naming the call it
/// cut short when there is one.
///
/// The backend may describe a cut call in its own terms ("the app returned
/// an error"), which reads as the app's fault rather than the caller's
/// timeout. What it said is kept as the last observation. The result is a
/// [`Error::Timeout`] with no delivery override, which reads as
/// [`Delivery::Unknown`](crate::Delivery::Unknown): the call was sent, and
/// may have taken effect.
pub(crate) fn action_error(err: Error) -> Error {
    let Some(deadline) = Deadline::current().filter(Deadline::is_expired) else {
        return err;
    };
    let Some(cut) = deadline.cut_short().map(str::to_owned) else {
        return err;
    };
    if err.diagnosis().is_some_and(|d| d.pending.is_some()) {
        return err;
    }
    let mut timeout = deadline.expired(Some(cut));
    if let Error::Timeout {
        diagnosis: Some(d), ..
    } = &mut timeout
    {
        d.last_observed = Some(format!("the platform reported: {err}"));
    }
    timeout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_scope_means_backend_default() {
        let t = call_timeout(Duration::from_secs(5), || unreachable!()).unwrap();
        assert_eq!(t.duration, Duration::from_secs(5));
        assert!(!t.is_deadline());
    }

    #[test]
    fn scope_caps_the_call_at_the_time_left() {
        Deadline::after(Duration::from_millis(500)).scope(|| {
            let t = call_timeout(Duration::from_secs(5), || unreachable!()).unwrap();
            assert!(t.duration <= Duration::from_millis(500));
            assert!(t.is_deadline());
        });
    }

    #[test]
    fn a_far_deadline_leaves_the_default() {
        Deadline::after(Duration::from_secs(60)).scope(|| {
            let t = call_timeout(Duration::from_secs(5), || unreachable!()).unwrap();
            assert_eq!(t.duration, Duration::from_secs(5));
            assert!(!t.is_deadline());
        });
    }

    #[test]
    fn an_expired_deadline_refuses_the_call_and_names_it() {
        Deadline::after(Duration::from_millis(1)).scope(|| {
            std::thread::sleep(Duration::from_millis(5));
            let err =
                call_timeout(Duration::from_secs(5), || "GetChildren on /x".into()).unwrap_err();
            let d = err.diagnosis().expect("diagnosed");
            assert_eq!(d.pending.as_deref(), Some("GetChildren on /x"));
            assert!(
                matches!(err, Error::Timeout { elapsed, .. } if elapsed == Duration::from_millis(1))
            );
            assert!(
                err.to_string().contains("still pending: GetChildren on /x"),
                "{err}"
            );
        });
    }

    #[test]
    fn nested_scopes_keep_the_earlier_deadline_and_restore() {
        let outer = Deadline::after(Duration::from_millis(200));
        outer.clone().scope(|| {
            Deadline::after(Duration::from_secs(30)).scope(|| {
                assert_eq!(Deadline::current().as_ref(), Some(&outer));
            });
            let inner = Deadline::after(Duration::from_millis(10));
            inner
                .clone()
                .scope(|| assert_eq!(Deadline::current(), Some(inner)));
            assert_eq!(Deadline::current().as_ref(), Some(&outer));
        });
        assert_eq!(Deadline::current(), None);
    }

    #[test]
    fn scope_restores_on_unwind() {
        let r = std::panic::catch_unwind(|| {
            Deadline::after(Duration::from_secs(1)).scope(|| panic!("boom"));
        });
        assert!(r.is_err());
        assert_eq!(Deadline::current(), None);
    }

    #[test]
    fn scope_opt_carries_a_deadline_across_threads() {
        let d = Deadline::after(Duration::from_secs(1));
        d.clone().scope(|| {
            let captured = Deadline::current();
            std::thread::spawn(move || {
                assert_eq!(Deadline::current(), None);
                Deadline::scope_opt(captured, || {
                    assert_eq!(Deadline::current(), Some(d));
                    // A cut reported on the worker is visible to the caller.
                    Deadline::current().map(|d| d.expired(Some("GetState on /w".into())));
                });
            })
            .join()
            .unwrap();
            assert_eq!(
                Deadline::current().and_then(|d| d.cut_short().map(str::to_owned)),
                Some("GetState on /w".to_owned())
            );
        });
    }

    #[test]
    fn diagnosis_gets_a_fresh_budget_past_an_expired_deadline() {
        Deadline::after(Duration::from_millis(1)).scope(|| {
            std::thread::sleep(Duration::from_millis(5));
            for_diagnosis(|| {
                let t = call_timeout(Duration::from_secs(5), || unreachable!()).unwrap();
                assert!(t.duration > Duration::from_secs(1));
            });
            assert!(Deadline::current().is_some_and(|d| d.is_expired()));
        });
    }

    #[test]
    fn zero_timeout_opens_no_scope() {
        within(Duration::ZERO, || assert_eq!(Deadline::current(), None));
        within(Duration::from_secs(1), || {
            assert!(Deadline::current().is_some())
        });
    }
}

/// The waits in core against a provider whose reads hang, the way a stopped
/// application's do: each takes the backend's whole native bound unless the
/// deadline caps it.
#[cfg(test)]
mod wait_tests {
    use super::*;
    use crate::element::ElementData;
    use crate::{App, Locator, Provider, Result};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// The fixed native bound a real backend would use for one call.
    const NATIVE: Duration = Duration::from_secs(5);

    struct HungProvider {
        inner: Arc<crate::mock::MockProvider>,
        /// When set, every read and action hangs for its whole bound.
        hung: AtomicBool,
        /// When set, a cut call surfaces as a platform error, the way a
        /// backend call site that maps every failure the same way would
        /// report it.
        as_platform: AtomicBool,
        /// When set, a cut children read returns what it had (nothing), the
        /// way a backend that tolerates a failed field does.
        degrade: AtomicBool,
        /// When set, reads answer and only actions hang.
        actions_only: AtomicBool,
        /// When set (with `hung`), the pre-action liveness check hangs.
        revalidate_hangs: AtomicBool,
    }

    impl HungProvider {
        /// One native call, as a backend makes it: bounded by
        /// [`call_timeout`], and reporting a deadline expiry with the call
        /// named as pending.
        fn native(&self, call: &str) -> Result<()> {
            let t = call_timeout(NATIVE, || call.to_string())?;
            if !self.hung.load(Ordering::SeqCst) {
                return Ok(());
            }
            std::thread::sleep(t.duration);
            let err = t.expired(call).unwrap_or_else(|| Error::timeout(NATIVE));
            if self.as_platform.load(Ordering::SeqCst) {
                return Err(Error::platform(-1, format!("{call} failed: {err}")));
            }
            Err(err)
        }
    }

    macro_rules! forward {
        ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
            $(fn $name(&self, e: &ElementData $(, $arg: $ty)*) -> Result<$ret> {
                self.native(concat!(stringify!($name), " on the target"))?;
                self.inner.$name(e $(, $arg)*)
            })*
        };
    }

    impl Provider for HungProvider {
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            if !self.degrade.load(Ordering::SeqCst) && !self.actions_only.load(Ordering::SeqCst) {
                self.native("list_apps on the registry")?;
            }
            self.inner.list_apps()
        }
        fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
            if self.degrade.load(Ordering::SeqCst) {
                // The read is cut, and the backend shrugs it off.
                let _ = self.native("get_children on the hung app");
                return Ok(Vec::new());
            }
            if !self.actions_only.load(Ordering::SeqCst) {
                self.native("get_children on the hung app")?;
            }
            self.inner.get_children(element)
        }
        fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
            self.inner.get_parent(element)
        }
        fn revalidate(&self, element: &ElementData) -> Result<()> {
            if self.revalidate_hangs.load(Ordering::SeqCst) {
                self.native("revalidate on the target")?;
            }
            self.inner.revalidate(element)
        }
        fn focused_app(&self) -> Result<ElementData> {
            self.inner.focused_app()
        }
        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
            self.inner.list_shell_surfaces()
        }
        forward! {
            press() -> (); focus() -> (); blur() -> (); toggle() -> (); select() -> ();
            expand() -> (); collapse() -> (); show_menu() -> (); increment() -> ();
            decrement() -> (); scroll_into_view() -> (); set_value(v: &str) -> ();
            set_numeric_value(v: f64) -> (); type_text(t: &str) -> ();
            set_text_selection(s: u32, f: u32) -> (); perform_action(a: &str) -> ();
            activate() -> (); minimize() -> (); maximize() -> (); enter_fullscreen() -> ();
            restore() -> (); close() -> (); move_to(x: i32, y: i32) -> ();
            resize_to(w: u32, h: u32) -> ();
            subscribe() -> crate::event_provider::Subscription;
        }
    }

    fn provider(hung: bool) -> Arc<HungProvider> {
        Arc::new(HungProvider {
            inner: crate::mock::build_provider(),
            hung: AtomicBool::new(hung),
            as_platform: AtomicBool::new(false),
            degrade: AtomicBool::new(false),
            actions_only: AtomicBool::new(false),
            revalidate_hangs: AtomicBool::new(false),
        })
    }

    const BUDGET: Duration = Duration::from_millis(300);
    /// Scheduling slack on a loaded CI runner. Far below [`NATIVE`], so a
    /// wait that let a read run its native bound fails the bound check.
    const SLACK: Duration = Duration::from_millis(1500);

    fn assert_bounded(start: std::time::Instant) {
        let took = start.elapsed();
        assert!(
            took < BUDGET + SLACK,
            "took {took:?} against a {BUDGET:?} timeout"
        );
    }

    #[test]
    fn a_wait_is_bounded_by_its_timeout_when_reads_hang() {
        let p = provider(true);
        let loc = Locator::new(p, None, r#"button[name="Back"]"#);
        let start = std::time::Instant::now();
        let err = loc.wait_visible(BUDGET).expect_err("every read hangs");
        assert_bounded(start);
        let Error::Timeout { .. } = &err else {
            panic!("expected a timeout, got {err:?}");
        };
        let d = err.diagnosis().expect("diagnosed");
        assert_eq!(d.selector.as_deref(), Some(r#"button[name="Back"]"#));
        assert_eq!(d.condition.as_deref(), Some("visible"));
        let pending = d.pending.as_deref().expect("names the read in flight");
        assert!(pending.contains(" on "), "{pending}");
        assert!(err.to_string().contains("still pending: "), "{err}");
        // A never-matched timeout keeps its scope snapshot (tenet 6), even
        // when the snapshot's own reads hang and can only report that.
        assert!(d.scope.is_some(), "{d:?}");
    }

    #[test]
    fn a_cut_read_reported_as_another_error_still_names_the_call() {
        let p = provider(true);
        p.as_platform.store(true, Ordering::SeqCst);
        let loc = Locator::new(p, None, r#"button[name="Back"]"#);
        let start = std::time::Instant::now();
        let err = loc.wait_visible(BUDGET).expect_err("every read hangs");
        assert_bounded(start);
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        // The first read the wait makes is the one that is cut.
        assert_eq!(
            err.diagnosis().and_then(|d| d.pending.as_deref()),
            Some("list_apps on the registry")
        );
    }

    #[test]
    fn a_cut_read_that_returned_nothing_is_not_an_observation() {
        let p = provider(true);
        p.degrade.store(true, Ordering::SeqCst);
        // `list_apps` must answer, so only the children read is cut.
        let loc = Locator::new(p.clone(), None, r#"button[name="Back"]"#);
        let start = std::time::Instant::now();
        // Without the guard the empty read would satisfy the wait at once.
        let err = loc
            .wait_detached(BUDGET)
            .expect_err("the read was cut, not answered");
        assert_bounded(start);
        assert_eq!(
            err.diagnosis().and_then(|d| d.pending.as_deref()),
            Some("get_children on the hung app")
        );
    }

    #[test]
    fn a_cut_action_is_a_timeout_whose_delivery_is_unknown() {
        let p = provider(false);
        let loc = Locator::new(p.clone(), None, r#"button[name="Back"]"#).with_timeout(BUDGET);
        loc.element().expect("resolves");
        // Only the press hangs, and the backend words it as its own failure.
        p.hung.store(true, Ordering::SeqCst);
        p.as_platform.store(true, Ordering::SeqCst);
        p.actions_only.store(true, Ordering::SeqCst);
        let err = loc.press().expect_err("the press hangs");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        assert_eq!(err.delivery(), crate::Delivery::Unknown);
        let d = err.diagnosis().expect("diagnosed");
        assert_eq!(d.pending.as_deref(), Some("press on the target"));
        assert!(d
            .last_observed
            .as_deref()
            .is_some_and(|o| o.starts_with("the platform reported: ")
                && o.contains("press on the target failed")));
    }

    #[test]
    fn a_cut_liveness_check_is_a_timeout_that_dispatched_nothing() {
        for as_platform in [false, true] {
            let p = provider(false);
            let loc = Locator::new(p.clone(), None, r#"button[name="Back"]"#).with_timeout(BUDGET);
            loc.element().expect("resolves");
            // Only the check before the press hangs.
            p.hung.store(true, Ordering::SeqCst);
            p.as_platform.store(as_platform, Ordering::SeqCst);
            p.actions_only.store(true, Ordering::SeqCst);
            p.revalidate_hangs.store(true, Ordering::SeqCst);
            let err = loc.press().expect_err("the check hangs");
            assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
            assert_eq!(err.delivery(), crate::Delivery::NotDelivered, "{err:?}");
            assert_eq!(
                err.diagnosis().and_then(|d| d.pending.as_deref()),
                Some("revalidate on the target")
            );
            assert!(
                !p.inner.actions().iter().any(|a| a.1 == "press"),
                "nothing was dispatched"
            );
        }
    }

    #[test]
    fn an_action_is_bounded_including_its_platform_call() {
        let p = provider(false);
        let loc = Locator::new(p.clone(), None, r#"button[name="Back"]"#).with_timeout(BUDGET);
        // The wait resolves at once; then the press itself hangs.
        let resolved = loc.element().expect("resolves");
        p.hung.store(true, Ordering::SeqCst);
        let start = std::time::Instant::now();
        let err = Deadline::after(BUDGET)
            .scope(|| resolved.press())
            .expect_err("the press hangs");
        assert_bounded(start);
        assert_eq!(
            err.diagnosis().and_then(|d| d.pending.as_deref()),
            Some("press on the target")
        );
        // Through the locator, the whole action shares one deadline.
        let start = std::time::Instant::now();
        loc.press().expect_err("hangs");
        assert_bounded(start);
    }

    #[test]
    fn an_app_lookup_is_bounded_and_names_the_pending_read() {
        let p = provider(true);
        let start = std::time::Instant::now();
        let err = App::by_name_with(p, "Test App", BUDGET).expect_err("enumeration hangs");
        assert_bounded(start);
        let Error::Timeout { .. } = &err else {
            panic!("expected a timeout, got {err:?}");
        };
        assert_eq!(
            err.diagnosis().and_then(|d| d.pending.as_deref()),
            Some("list_apps on the registry")
        );
    }

    #[test]
    fn a_zero_timeout_keeps_the_native_bound_for_its_single_attempt() {
        // No deadline opens, so the one attempt's reads are not refused for
        // running past a zero budget.
        let p = provider(false);
        let loc = Locator::new(p, None, r#"button[name="Back"]"#).with_timeout(Duration::ZERO);
        loc.press().expect("one attempt succeeds");
    }

    #[test]
    fn a_healthy_provider_is_unaffected() {
        let p = provider(false);
        let loc = Locator::new(p, None, r#"button[name="Back"]"#);
        loc.wait_visible(BUDGET).expect("visible");
    }
}
