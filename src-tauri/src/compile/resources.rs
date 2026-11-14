//! Aggregate reservations of configured guest RAM and vCPUs, not host usage.
//! Keep one lease per VM from before boot until it has fully stopped, including
//! preparation VMs and booting, running, warm, and stopping workers.
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Amount {
    pub cpus: u32,
    #[serde(rename = "memoryMiB")]
    pub memory_mib: u64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub limit: Amount,
    pub allocated: Amount,
    pub vm_count: usize,
}

/// A shared budget for every VM lifetime managed by the caller.
pub struct Pool {
    state: Mutex<State>,
}

struct State {
    limit: Amount,
    allocated: Amount,
    vm_count: usize,
}

impl State {
    fn after_reservation(&self, amount: Amount) -> Option<(Amount, usize)> {
        if amount.cpus == 0 || amount.memory_mib == 0 {
            return None;
        }
        let allocated = Amount {
            cpus: self.allocated.cpus.checked_add(amount.cpus)?,
            memory_mib: self.allocated.memory_mib.checked_add(amount.memory_mib)?,
        };
        if allocated.cpus > self.limit.cpus || allocated.memory_mib > self.limit.memory_mib {
            return None;
        }
        Some((allocated, self.vm_count.checked_add(1)?))
    }
}

impl Pool {
    pub fn new(limit: Amount) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                limit,
                allocated: Amount {
                    cpus: 0,
                    memory_mib: 0,
                },
                vm_count: 0,
            }),
        })
    }

    // A panicking persistence callback cannot change the accounting, so leases
    // can still be released if the caller catches that panic.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Atomically reserves both resources. Either zero amount is rejected.
    pub fn try_reserve(self: &Arc<Self>, amount: Amount) -> Option<Arc<Lease>> {
        let mut state = self.state();
        let (allocated, vm_count) = state.after_reservation(amount)?;
        state.allocated = allocated;
        state.vm_count = vm_count;
        Some(Arc::new(Lease {
            pool: Arc::clone(self),
            amount,
        }))
    }

    /// A read-only admission hint; only `try_reserve` guarantees capacity.
    pub fn can_reserve(&self, amount: Amount) -> bool {
        self.state().after_reservation(amount).is_some()
    }

    /// Persists and changes the limit without interrupting outstanding leases.
    ///
    /// `persist` runs while the accounting mutex is held and must not call back
    /// into this pool (including through dropping one of its leases), or wait
    /// for work that needs the pool. On error, the current limit is unchanged.
    pub fn reconfigure(
        &self,
        new_limit: Amount,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.state();
        if state.allocated.cpus > new_limit.cpus
            || state.allocated.memory_mib > new_limit.memory_mib
        {
            return Err(format!(
                "Cannot lower the VM budget: allocated VM capacity is {} vCPUs / {} MiB, \
                 exceeding the requested {} vCPUs / {} MiB. Stop VMs before lowering the budget.",
                state.allocated.cpus,
                state.allocated.memory_mib,
                new_limit.cpus,
                new_limit.memory_mib,
            ));
        }
        persist()?;
        state.limit = new_limit;
        Ok(())
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.state();
        Snapshot {
            limit: state.limit,
            allocated: state.allocated,
            vm_count: state.vm_count,
        }
    }
}

/// Capacity is returned only when the last `Arc<Lease>` is dropped.
/// The lease also keeps its pool alive.
pub struct Lease {
    pool: Arc<Pool>,
    amount: Amount,
}

impl Lease {
    pub fn amount(&self) -> Amount {
        self.amount
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.pool.state();
        state.allocated.cpus -= self.amount.cpus;
        state.allocated.memory_mib -= self.amount.memory_mib;
        state.vm_count -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Barrier, thread};

    fn amount(cpus: u32, memory_mib: u64) -> Amount {
        Amount { cpus, memory_mib }
    }

    #[test]
    fn admission_uses_sum_of_both_resources() {
        let pool = Pool::new(amount(8, 2048));
        let first = pool.try_reserve(amount(2, 768)).unwrap();
        let second = pool.try_reserve(amount(4, 1024)).unwrap();
        assert_eq!(pool.snapshot().allocated, amount(6, 1792));
        assert_eq!(pool.snapshot().vm_count, 2);
        assert!(pool.try_reserve(amount(3, 1)).is_none());
        assert!(pool.try_reserve(amount(1, 257)).is_none());
        assert!(pool.can_reserve(amount(2, 256)));
        let third = pool.try_reserve(amount(2, 256)).unwrap();
        assert_eq!(pool.snapshot().allocated, amount(8, 2048));
        assert_eq!(pool.snapshot().vm_count, 3);
        assert!(!pool.can_reserve(amount(1, 1)));
        drop((first, second, third));
        assert_eq!(pool.snapshot().allocated, amount(0, 0));
        assert_eq!(pool.snapshot().vm_count, 0);
    }

    #[test]
    fn rejects_zero_and_overflow_without_changing_accounting() {
        let pool = Pool::new(amount(u32::MAX, u64::MAX));
        for invalid in [amount(0, 0), amount(0, 1), amount(1, 0)] {
            assert!(!pool.can_reserve(invalid));
            assert!(pool.try_reserve(invalid).is_none());
        }
        let lease = pool
            .try_reserve(amount(u32::MAX - 1, u64::MAX - 1))
            .unwrap();
        let before = pool.snapshot();
        for overflowing in [amount(2, 1), amount(1, 2)] {
            assert!(!pool.can_reserve(overflowing));
            assert!(pool.try_reserve(overflowing).is_none());
            assert_eq!(pool.snapshot(), before);
        }
        drop(lease);
        assert_eq!(pool.snapshot().allocated, amount(0, 0));
    }

    #[test]
    fn last_clone_returns_capacity() {
        let pool = Pool::new(amount(2, 512));
        let lease = pool.try_reserve(amount(2, 512)).unwrap();
        let clone = Arc::clone(&lease);
        assert_eq!(lease.amount(), amount(2, 512));
        drop(lease);
        assert_eq!(pool.snapshot().allocated, amount(2, 512));
        assert_eq!(pool.snapshot().vm_count, 1);
        assert!(pool.try_reserve(amount(1, 1)).is_none());
        drop(clone);
        assert_eq!(pool.snapshot().allocated, amount(0, 0));
        assert_eq!(pool.snapshot().vm_count, 0);
        assert!(pool.try_reserve(amount(2, 512)).is_some());
    }

    #[test]
    fn concurrent_admission_respects_cpu_and_memory_totals() {
        // Exercise CPU-bound and RAM-bound admission independently. Successful
        // leases stay alive until every contender has tried to reserve.
        for (limit, requested) in [
            (amount(7, 1000), amount(2, 100)),
            (amount(16, 1000), amount(1, 300)),
        ] {
            let pool = Pool::new(limit);
            let barrier = Arc::new(Barrier::new(16));
            let threads: Vec<_> = (0..16)
                .map(|_| {
                    let pool = Arc::clone(&pool);
                    let barrier = Arc::clone(&barrier);
                    thread::spawn(move || {
                        barrier.wait();
                        let lease = pool.try_reserve(requested);
                        let snapshot = pool.snapshot();
                        barrier.wait();
                        assert!(snapshot.allocated.cpus <= limit.cpus);
                        assert!(snapshot.allocated.memory_mib <= limit.memory_mib);
                        lease
                    })
                })
                .collect();
            let leases: Vec<_> = threads
                .into_iter()
                .filter_map(|thread| thread.join().unwrap())
                .collect();
            assert_eq!(leases.len(), 3);
            assert_eq!(pool.snapshot().vm_count, 3);
            assert_eq!(
                pool.snapshot().allocated,
                amount(requested.cpus * 3, requested.memory_mib * 3)
            );
            drop(leases);
            assert_eq!(pool.snapshot().allocated, amount(0, 0));
            assert_eq!(pool.snapshot().vm_count, 0);
        }
    }

    #[test]
    fn failed_persistence_leaves_limit_unchanged() {
        let pool = Pool::new(amount(4, 1024));
        let _lease = pool.try_reserve(amount(2, 512)).unwrap();
        let before = pool.snapshot();
        let result = pool.reconfigure(amount(8, 2048), || Err("write failed".into()));
        assert_eq!(result, Err("write failed".into()));
        assert_eq!(pool.snapshot(), before);
        assert!(pool.try_reserve(amount(3, 512)).is_none());
    }

    #[test]
    fn shrinking_below_allocated_rejects_before_persistence() {
        let pool = Pool::new(amount(8, 2048));
        let _lease = pool.try_reserve(amount(4, 1024)).unwrap();
        let before = pool.snapshot();
        for limit in [amount(3, 2048), amount(8, 1023)] {
            let mut persisted = false;
            let error = pool
                .reconfigure(limit, || {
                    persisted = true;
                    Ok(())
                })
                .unwrap_err();
            assert!(error.contains("allocated VM capacity"));
            assert!(!persisted);
            assert_eq!(pool.snapshot(), before);
        }
    }

    #[test]
    fn successful_changes_honor_outstanding_leases() {
        let pool = Pool::new(amount(8, 2048));
        let first = pool.try_reserve(amount(4, 1024)).unwrap();
        let mut persisted = false;
        pool.reconfigure(amount(4, 1024), || {
            persisted = true;
            Ok(())
        })
        .unwrap();
        assert!(persisted);
        assert_eq!(pool.snapshot().limit, amount(4, 1024));
        assert_eq!(pool.snapshot().allocated, first.amount());
        assert!(pool.try_reserve(amount(1, 1)).is_none());

        pool.reconfigure(amount(6, 1536), || Ok(())).unwrap();
        let second = pool.try_reserve(amount(2, 512)).unwrap();
        drop(first);
        assert_eq!(pool.snapshot().allocated, second.amount());
        assert_eq!(pool.snapshot().vm_count, 1);

        pool.reconfigure(amount(3, 768), || Ok(())).unwrap();
        assert!(pool.try_reserve(amount(2, 1)).is_none());
        assert!(pool.try_reserve(amount(1, 257)).is_none());
        drop(second);
        assert_eq!(
            pool.snapshot(),
            Snapshot {
                limit: amount(3, 768),
                allocated: amount(0, 0),
                vm_count: 0,
            }
        );
    }
}
