//! Cancellation-safe admission and drain for login wire work.
//!
//! Request admission and actor-owned outbound publication close in separate
//! phases. Graceful shutdown first retires every request while World and
//! profile services remain live, then seals producers and waits for every
//! queued write guard. Force shutdown closes both phases and wakes either
//! waiter without waiting for client or disk progress.

use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Notify;

#[derive(Debug, Clone)]
pub(crate) struct WireOperationGate {
    inner: Arc<WireOperationGateInner>,
}

#[derive(Debug)]
struct WireOperationGateInner {
    state: Mutex<WireOperationGateState>,
    changed: Notify,
}

#[derive(Debug)]
struct WireOperationGateState {
    accepting_requests: bool,
    accepting_outbound: bool,
    active_requests: usize,
    active_outbound: usize,
    force_bypassed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireOperationCounts {
    pub(crate) requests: usize,
    pub(crate) outbound: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireOperationKind {
    Request,
    Outbound,
}

impl WireOperationGate {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(WireOperationGateInner {
                state: Mutex::new(WireOperationGateState {
                    accepting_requests: true,
                    accepting_outbound: true,
                    active_requests: 0,
                    active_outbound: 0,
                    force_bypassed: false,
                }),
                changed: Notify::new(),
            }),
        }
    }

    /// Admits one fully decoded login request while listener-side admission is
    /// open. Counter exhaustion seals this phase rather than wrapping.
    pub(crate) fn try_begin_request(&self) -> Option<WireOperationGuard> {
        self.try_begin(WireOperationKind::Request)
    }

    /// Tracks one actor-owned batch from reservation through successful write
    /// or explicit transport failure. Producers remain open after request
    /// admission closes and are sealed only after their runtime barriers.
    pub(crate) fn try_begin_outbound(&self) -> Option<WireOperationGuard> {
        self.try_begin(WireOperationKind::Outbound)
    }

    fn try_begin(&self, kind: WireOperationKind) -> Option<WireOperationGuard> {
        let mut state = self.lock_state();
        let next = match kind {
            WireOperationKind::Request if state.accepting_requests => {
                state.active_requests.checked_add(1)
            }
            WireOperationKind::Outbound if state.accepting_outbound => {
                state.active_outbound.checked_add(1)
            }
            WireOperationKind::Request | WireOperationKind::Outbound => return None,
        };
        let Some(next) = next else {
            match kind {
                WireOperationKind::Request => state.accepting_requests = false,
                WireOperationKind::Outbound => state.accepting_outbound = false,
            }
            drop(state);
            self.inner.changed.notify_waiters();
            return None;
        };
        match kind {
            WireOperationKind::Request => state.active_requests = next,
            WireOperationKind::Outbound => state.active_outbound = next,
        }
        drop(state);
        Some(WireOperationGuard {
            inner: Some(Arc::clone(&self.inner)),
            kind,
        })
    }

    /// Permanently closes request admission and returns its exact active count
    /// at the close linearization point.
    pub(crate) fn close_request_admission(&self) -> usize {
        let active = {
            let mut state = self.lock_state();
            state.accepting_requests = false;
            state.active_requests
        };
        self.inner.changed.notify_waiters();
        active
    }

    /// Permanently closes actor-owned outbound admission and returns its exact
    /// active count at the close linearization point.
    pub(crate) fn close_outbound_admission(&self) -> usize {
        let active = {
            let mut state = self.lock_state();
            state.accepting_outbound = false;
            state.active_outbound
        };
        self.inner.changed.notify_waiters();
        active
    }

    /// Closes both phases and releases graceful waiters. Existing guards still
    /// retire normally so the abandoned count remains observable.
    pub(crate) fn force_bypass(&self) -> WireOperationCounts {
        let counts = {
            let mut state = self.lock_state();
            state.accepting_requests = false;
            state.accepting_outbound = false;
            state.force_bypassed = true;
            WireOperationCounts {
                requests: state.active_requests,
                outbound: state.active_outbound,
            }
        };
        self.inner.changed.notify_waiters();
        counts
    }

    /// Returns `true` only after every admitted request retires. `false`
    /// reports an operator force transition.
    pub(crate) async fn wait_for_request_drain_or_bypass(&self) -> bool {
        loop {
            let changed = self.inner.changed.notified();
            let (active, force_bypassed) = {
                let state = self.lock_state();
                (state.active_requests, state.force_bypassed)
            };
            if force_bypassed {
                return false;
            }
            if active == 0 {
                return true;
            }
            changed.await;
        }
    }

    /// Returns `true` only after every tracked outbound batch retires. `false`
    /// reports an operator force transition.
    pub(crate) async fn wait_for_outbound_drain_or_bypass(&self) -> bool {
        loop {
            let changed = self.inner.changed.notified();
            let (active, force_bypassed) = {
                let state = self.lock_state();
                (state.active_outbound, state.force_bypassed)
            };
            if force_bypassed {
                return false;
            }
            if active == 0 {
                return true;
            }
            changed.await;
        }
    }

    /// Lets an idle or partial-frame session transition to writer-only drain
    /// mode as soon as request admission closes.
    pub(crate) async fn wait_for_request_admission_close(&self) {
        loop {
            let changed = self.inner.changed.notified();
            let accepting_requests = self.lock_state().accepting_requests;
            if !accepting_requests {
                return;
            }
            changed.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn active_counts(&self) -> WireOperationCounts {
        let state = self.lock_state();
        WireOperationCounts {
            requests: state.active_requests,
            outbound: state.active_outbound,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, WireOperationGateState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug)]
pub(crate) struct WireOperationGuard {
    inner: Option<Arc<WireOperationGateInner>>,
    kind: WireOperationKind,
}

impl Drop for WireOperationGuard {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        let drained = {
            let mut state = inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active = match self.kind {
                WireOperationKind::Request => &mut state.active_requests,
                WireOperationKind::Outbound => &mut state.active_outbound,
            };
            let Some(next) = active.checked_sub(1) else {
                debug_assert!(false, "wire operation guard retired more than once");
                return;
            };
            *active = next;
            next == 0
        };
        if drained {
            inner.changed.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time;

    use super::{WireOperationCounts, WireOperationGate, WireOperationKind};

    #[tokio::test]
    async fn request_and_outbound_phases_close_and_drain_independently() {
        let gate = WireOperationGate::new();
        let request = gate.try_begin_request().expect("request admitted");
        let outbound = gate.try_begin_outbound().expect("outbound admitted");

        assert_eq!(gate.close_request_admission(), 1);
        assert!(gate.try_begin_request().is_none());
        assert!(gate.try_begin_outbound().is_some());
        let waiting_gate = gate.clone();
        let mut request_drain =
            tokio::spawn(async move { waiting_gate.wait_for_request_drain_or_bypass().await });
        assert!(
            time::timeout(Duration::from_millis(10), &mut request_drain)
                .await
                .is_err()
        );
        drop(request);
        assert!(
            time::timeout(Duration::from_secs(1), request_drain)
                .await
                .expect("request drain wakes")
                .expect("request drain task succeeds")
        );

        assert_eq!(gate.close_outbound_admission(), 1);
        let waiting_gate = gate.clone();
        let mut outbound_drain =
            tokio::spawn(async move { waiting_gate.wait_for_outbound_drain_or_bypass().await });
        assert!(
            time::timeout(Duration::from_millis(10), &mut outbound_drain)
                .await
                .is_err()
        );
        drop(outbound);
        assert!(
            time::timeout(Duration::from_secs(1), outbound_drain)
                .await
                .expect("outbound drain wakes")
                .expect("outbound drain task succeeds")
        );
    }

    #[tokio::test]
    async fn force_closes_both_phases_and_wakes_both_waiters() {
        let gate = WireOperationGate::new();
        let request = gate.try_begin_request().expect("request admitted");
        let outbound = gate.try_begin_outbound().expect("outbound admitted");
        let request_gate = gate.clone();
        let outbound_gate = gate.clone();
        let request_wait =
            tokio::spawn(async move { request_gate.wait_for_request_drain_or_bypass().await });
        let outbound_wait =
            tokio::spawn(async move { outbound_gate.wait_for_outbound_drain_or_bypass().await });

        assert_eq!(
            gate.force_bypass(),
            WireOperationCounts {
                requests: 1,
                outbound: 1
            }
        );
        assert!(!request_wait.await.expect("request waiter succeeds"));
        assert!(!outbound_wait.await.expect("outbound waiter succeeds"));
        assert!(gate.try_begin_request().is_none());
        assert!(gate.try_begin_outbound().is_none());
        drop(request);
        drop(outbound);
        assert_eq!(
            gate.active_counts(),
            WireOperationCounts {
                requests: 0,
                outbound: 0
            }
        );
    }

    #[test]
    fn counter_exhaustion_closes_only_the_exhausted_phase() {
        let gate = WireOperationGate::new();
        {
            let mut state = gate.lock_state();
            state.active_requests = usize::MAX;
        }
        assert!(gate.try_begin_request().is_none());
        assert!(!gate.lock_state().accepting_requests);
        assert!(gate.try_begin_outbound().is_some());

        {
            let mut state = gate.lock_state();
            state.active_requests = 0;
            state.active_outbound = usize::MAX;
        }
        assert!(gate.try_begin_outbound().is_none());
        assert!(!gate.lock_state().accepting_outbound);
    }

    #[test]
    fn operation_kinds_remain_distinct() {
        assert_ne!(WireOperationKind::Request, WireOperationKind::Outbound);
    }
}
