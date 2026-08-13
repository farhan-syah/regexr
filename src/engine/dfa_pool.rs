//! A small pool of [`LazyDfa`] instances, one per concurrent search.
//!
//! A lazy DFA builds its states on demand, so a search needs `&mut` access to
//! the state cache. Sharing one instance behind a lock therefore makes
//! concurrent searches on a single `Regex` contend for the whole duration of
//! every search. Handing each search its own instance removes the contention:
//! the pool lock is held only to take an instance out and to put it back.
//!
//! The cache is only a cache — every instance runs the same subset
//! construction over the same NFA — so pooling changes which instance computes
//! a state, never what that state is.

use std::sync::Mutex;

use crate::dfa::LazyDfa;

/// How many idle instances the pool keeps.
///
/// Each cached DFA can grow to its own cache limit in states, so the pool is
/// capped rather than left to grow with the peak thread count: a burst of
/// threads would otherwise leave that memory retained for the lifetime of the
/// `Regex`. Eight covers the common case of a handful of worker threads
/// without keeping the cache of a thread that ran once; past it, a search
/// still gets a correct instance by cloning the template, and drops it on
/// completion.
const MAX_IDLE: usize = 8;

/// Hands out [`LazyDfa`] instances for the duration of a single search.
pub(crate) struct LazyDfaPool {
    /// Cloned when no idle instance is available.
    ///
    /// Never searched, so its per-search state (`search_depth`,
    /// `ceiling_exceeded`) stays at the values a freshly built DFA has, and a
    /// clone of it behaves exactly like `LazyDfa::new` would — without
    /// recompiling the NFA or recomputing its epsilon closures.
    template: LazyDfa,
    idle: Mutex<Vec<LazyDfa>>,
}

impl LazyDfaPool {
    /// Creates a pool that hands out clones of `template`.
    pub(crate) fn new(template: LazyDfa) -> Self {
        Self {
            template,
            idle: Mutex::new(Vec::new()),
        }
    }

    /// Takes an instance out of the pool, cloning the template when none is
    /// idle.
    ///
    /// The caller owns it until it is handed back with [`Self::checkin`];
    /// simply dropping it is also correct, and only costs the cached states.
    pub(crate) fn checkout(&self) -> LazyDfa {
        match self.idle.lock().unwrap().pop() {
            Some(dfa) => dfa,
            None => self.template.clone(),
        }
    }

    /// Returns an instance for the next search to reuse, or drops it when the
    /// pool already holds [`MAX_IDLE`].
    pub(crate) fn checkin(&self, dfa: LazyDfa) {
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < MAX_IDLE {
            idle.push(dfa);
        }
    }

    /// Runs `search` on an instance owned exclusively by this call.
    ///
    /// The instance is returned to the pool only on a normal return. If
    /// `search` unwinds, the instance is dropped with it rather than being
    /// handed to the next search in whatever state the panic left it.
    pub(crate) fn with<T>(&self, search: impl FnOnce(&mut LazyDfa) -> T) -> T {
        let mut dfa = self.checkout();
        let result = search(&mut dfa);
        self.checkin(dfa);
        result
    }
}
