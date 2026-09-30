//! Running effects without an async runtime: a small worker pool, a timer
//! thread, and a handle for sending messages back.

use std::collections::{BinaryHeap, VecDeque};
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::runtime::Input;

type Job = Box<dyn FnOnce() + Send>;

/// Most worker threads the pool starts. Jobs beyond that wait in a queue.
pub(crate) const MAX_WORKERS: usize = 8;
/// How long an idle worker waits for work before it exits.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// The program has ended, so a message can't be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl fmt::Display for Closed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the program has stopped")
    }
}

impl std::error::Error for Closed {}

/// Sends messages to a running [`Program`](crate::Program) from any thread.
///
/// Every message goes through the app's `update`, in the order it was sent
/// by one thread, and wakes the loop right away. Once the program has
/// ended, `send` returns [`Closed`], which is how a worker that streams
/// results finds out it should stop.
pub struct Sender<M> {
    tx: mpsc::Sender<Input<M>>,
}

impl<M> Sender<M> {
    pub(crate) fn new(tx: mpsc::Sender<Input<M>>) -> Self {
        Sender { tx }
    }

    /// Queues `message` for `update`.
    pub fn send(&self, message: M) -> Result<(), Closed> {
        self.tx.send(Input::Message(message)).map_err(|_| Closed)
    }
}

impl<M> Clone for Sender<M> {
    fn clone(&self) -> Self {
        Sender {
            tx: self.tx.clone(),
        }
    }
}

impl<M> fmt::Debug for Sender<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sender")
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A job that panicked can't leave the queue half-updated, so a
    // poisoned lock is still safe to use.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct PoolState {
    queue: VecDeque<Job>,
    workers: usize,
    idle: usize,
    closed: bool,
}

/// Runs blocking jobs on at most `max` threads. Threads start on demand
/// and exit after sitting idle.
pub(crate) struct Pool {
    state: Mutex<PoolState>,
    wake: Condvar,
    max: usize,
    idle_timeout: Duration,
}

impl Pool {
    pub(crate) fn new(max: usize, idle_timeout: Duration) -> Arc<Pool> {
        Arc::new(Pool {
            state: Mutex::new(PoolState {
                queue: VecDeque::new(),
                workers: 0,
                idle: 0,
                closed: false,
            }),
            wake: Condvar::new(),
            max: max.max(1),
            idle_timeout,
        })
    }

    pub(crate) fn execute(self: &Arc<Pool>, job: Job) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        state.queue.push_back(job);
        if state.idle > 0 {
            self.wake.notify_one();
        }
        // A woken worker is still counted as idle until it takes the lock
        // again, so `idle` alone can't say whether the queue is covered.
        // Start another worker whenever there are more jobs than idle ones.
        if state.queue.len() > state.idle && state.workers < self.max {
            state.workers += 1;
            let pool = Arc::clone(self);
            let started = thread::Builder::new()
                .name("crewtui-worker".into())
                .spawn(move || pool.work());
            if started.is_err() {
                state.workers -= 1;
            }
        }
    }

    fn work(self: Arc<Pool>) {
        let mut state = lock(&self.state);
        loop {
            if let Some(job) = state.queue.pop_front() {
                drop(state);
                // A panicking job must not take the worker down with it.
                let _ = panic::catch_unwind(AssertUnwindSafe(job));
                state = lock(&self.state);
                continue;
            }
            if state.closed {
                break;
            }
            state.idle += 1;
            let (guard, timeout) = self
                .wake
                .wait_timeout(state, self.idle_timeout)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
            state.idle -= 1;
            if timeout.timed_out() && state.queue.is_empty() {
                break;
            }
        }
        state.workers -= 1;
    }

    /// Stops accepting jobs. Workers finish what they are running and exit;
    /// jobs still queued are dropped.
    pub(crate) fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.queue.clear();
        self.wake.notify_all();
    }

    #[cfg(test)]
    fn workers(&self) -> usize {
        lock(&self.state).workers
    }

    #[cfg(test)]
    fn idle(&self) -> usize {
        lock(&self.state).idle
    }
}

struct Entry {
    at: Instant,
    seq: u64,
    job: Job,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.seq) == (other.at, other.seq)
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    /// Reversed, so the heap pops the earliest deadline first.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (other.at, other.seq).cmp(&(self.at, self.seq))
    }
}

struct TimerState {
    heap: BinaryHeap<Entry>,
    next_seq: u64,
    started: bool,
    closed: bool,
}

/// Runs jobs at a deadline, on one thread that sleeps until the next one.
pub(crate) struct Timers {
    state: Arc<(Mutex<TimerState>, Condvar)>,
}

impl Timers {
    pub(crate) fn new() -> Timers {
        Timers {
            state: Arc::new((
                Mutex::new(TimerState {
                    heap: BinaryHeap::new(),
                    next_seq: 0,
                    started: false,
                    closed: false,
                }),
                Condvar::new(),
            )),
        }
    }

    pub(crate) fn add(&self, at: Instant, job: Job) {
        let (mutex, wake) = &*self.state;
        let mut state = lock(mutex);
        if state.closed {
            return;
        }
        let seq = state.next_seq;
        state.next_seq += 1;
        state.heap.push(Entry { at, seq, job });
        if !state.started {
            state.started = true;
            let shared = Arc::clone(&self.state);
            let spawned = thread::Builder::new()
                .name("crewtui-timers".into())
                .spawn(move || Timers::run(&shared));
            if spawned.is_err() {
                state.started = false;
            }
        }
        wake.notify_one();
    }

    fn run(shared: &(Mutex<TimerState>, Condvar)) {
        let (mutex, wake) = shared;
        let mut state = lock(mutex);
        loop {
            if state.closed {
                return;
            }
            let now = Instant::now();
            match state.heap.peek().map(|e| e.at) {
                None => state = wake.wait(state).unwrap_or_else(PoisonError::into_inner),
                Some(at) if at <= now => {
                    let Some(entry) = state.heap.pop() else {
                        continue;
                    };
                    drop(state);
                    let _ = panic::catch_unwind(AssertUnwindSafe(entry.job));
                    state = lock(mutex);
                }
                Some(at) => {
                    state = wake
                        .wait_timeout(state, at - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
    }
}

impl Drop for Timers {
    /// Pending timers are dropped, which is how they are cancelled when the
    /// program ends.
    fn drop(&mut self) {
        let (mutex, wake) = &*self.state;
        let mut state = lock(mutex);
        state.closed = true;
        state.heap.clear();
        wake.notify_all();
    }
}

/// Where a running program's effects execute.
pub(crate) struct Effects<M> {
    tx: mpsc::Sender<Input<M>>,
    pool: Arc<Pool>,
    timers: Timers,
}

impl<M: Send + 'static> Effects<M> {
    pub(crate) fn new(tx: mpsc::Sender<Input<M>>) -> Self {
        Effects {
            tx,
            pool: Pool::new(MAX_WORKERS, IDLE_TIMEOUT),
            timers: Timers::new(),
        }
    }

    pub(crate) fn perform(&self, f: Box<dyn FnOnce() -> M + Send>) {
        let tx = self.tx.clone();
        self.pool.execute(Box::new(move || {
            let _ = tx.send(Input::Message(f()));
        }));
    }

    pub(crate) fn spawn(&self, f: Box<dyn FnOnce(Sender<M>) + Send>) {
        let sender = Sender::new(self.tx.clone());
        self.pool.execute(Box::new(move || f(sender)));
    }

    pub(crate) fn after(&self, delay: Duration, message: M) {
        // A delay too large to represent, such as `Duration::MAX` used to
        // mean "never", is a timer that never fires.
        let Some(at) = Instant::now().checked_add(delay) else {
            return;
        };
        let tx = self.tx.clone();
        self.timers.add(
            at,
            Box::new(move || {
                let _ = tx.send(Input::Message(message));
            }),
        );
    }
}

impl<M> Drop for Effects<M> {
    fn drop(&mut self) {
        self.pool.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::channel;

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn the_pool_never_runs_more_jobs_at_once_than_its_limit() {
        let pool = Pool::new(4, Duration::from_secs(5));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        for _ in 0..40 {
            let (running, peak, finished) = (running.clone(), peak.clone(), finished.clone());
            pool.execute(Box::new(move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                running.fetch_sub(1, Ordering::SeqCst);
                finished.fetch_add(1, Ordering::SeqCst);
            }));
        }
        wait_until("all jobs", || finished.load(Ordering::SeqCst) == 40);
        assert!(
            peak.load(Ordering::SeqCst) <= 4,
            "peak {}",
            peak.load(Ordering::SeqCst)
        );
        assert!(pool.workers() <= 4);
    }

    #[test]
    fn idle_workers_exit_and_new_work_starts_new_ones() {
        let pool = Pool::new(4, Duration::from_millis(30));
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..4 {
            let done = done.clone();
            pool.execute(Box::new(move || {
                done.fetch_add(1, Ordering::SeqCst);
            }));
        }
        wait_until("jobs", || done.load(Ordering::SeqCst) == 4);
        wait_until("idle exit", || pool.workers() == 0);
        let done2 = done.clone();
        pool.execute(Box::new(move || {
            done2.fetch_add(1, Ordering::SeqCst);
        }));
        wait_until("a new worker", || done.load(Ordering::SeqCst) == 5);
    }

    #[test]
    fn a_second_job_does_not_wait_behind_a_long_one_while_slots_are_free() {
        let pool = Pool::new(4, Duration::from_secs(5));
        // Leave exactly one worker idle.
        let warm = Arc::new(AtomicBool::new(false));
        let w = warm.clone();
        pool.execute(Box::new(move || w.store(true, Ordering::SeqCst)));
        wait_until("the warm-up job", || warm.load(Ordering::SeqCst));
        wait_until("the worker to go idle", || pool.idle() == 1);

        let gate = Arc::new(AtomicBool::new(false));
        let g = gate.clone();
        pool.execute(Box::new(move || {
            while !g.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
        }));
        let second = Arc::new(AtomicBool::new(false));
        let s2 = second.clone();
        pool.execute(Box::new(move || s2.store(true, Ordering::SeqCst)));
        // The long job still holds one worker; the second must get another.
        wait_until("the second job to run beside the long one", || {
            second.load(Ordering::SeqCst)
        });
        gate.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_panicking_job_does_not_kill_its_worker() {
        let pool = Pool::new(1, Duration::from_secs(5));
        pool.execute(Box::new(|| panic!("job panic")));
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        pool.execute(Box::new(move || flag.store(true, Ordering::SeqCst)));
        wait_until("the second job", || ran.load(Ordering::SeqCst));
        assert_eq!(pool.workers(), 1);
    }

    #[test]
    fn a_closed_pool_drops_queued_jobs_and_refuses_new_ones() {
        let pool = Pool::new(1, Duration::from_secs(5));
        let gate = Arc::new(AtomicBool::new(false));
        let ran = Arc::new(AtomicBool::new(false));
        let g = gate.clone();
        pool.execute(Box::new(move || {
            while !g.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
        }));
        let r = ran.clone();
        pool.execute(Box::new(move || r.store(true, Ordering::SeqCst)));
        pool.close();
        let r = ran.clone();
        pool.execute(Box::new(move || r.store(true, Ordering::SeqCst)));
        gate.store(true, Ordering::SeqCst);
        wait_until("the worker to leave", || pool.workers() == 0);
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn timers_fire_in_deadline_order_and_not_early() {
        let timers = Timers::new();
        let (tx, rx) = channel();
        let start = Instant::now();
        for (ms, tag) in [(60, 'b'), (10, 'a'), (110, 'c')] {
            let tx = tx.clone();
            timers.add(
                start + Duration::from_millis(ms),
                Box::new(move || {
                    tx.send((tag, Instant::now())).unwrap();
                }),
            );
        }
        let got: Vec<_> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(got.iter().map(|g| g.0).collect::<String>(), "abc");
        assert!(got[0].1 - start >= Duration::from_millis(10));
        assert!(got[2].1 - start >= Duration::from_millis(110));
    }

    #[test]
    fn equal_deadlines_fire_in_the_order_they_were_added() {
        let timers = Timers::new();
        let (tx, rx) = channel();
        let at = Instant::now() + Duration::from_millis(20);
        for tag in 0..20 {
            let tx = tx.clone();
            timers.add(at, Box::new(move || tx.send(tag).unwrap()));
        }
        let got: Vec<i32> = (0..20)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(got, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn an_earlier_timer_added_later_wakes_the_sleeping_thread() {
        let timers = Timers::new();
        let (tx, rx) = channel();
        let far = tx.clone();
        timers.add(
            Instant::now() + Duration::from_secs(30),
            Box::new(move || far.send('x').unwrap()),
        );
        thread::sleep(Duration::from_millis(20));
        timers.add(
            Instant::now() + Duration::from_millis(10),
            Box::new(move || tx.send('y').unwrap()),
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 'y');
    }

    #[test]
    fn dropping_the_timers_cancels_whatever_is_pending() {
        let timers = Timers::new();
        let fired = Arc::new(AtomicBool::new(false));
        let f = fired.clone();
        timers.add(
            Instant::now() + Duration::from_millis(40),
            Box::new(move || f.store(true, Ordering::SeqCst)),
        );
        drop(timers);
        thread::sleep(Duration::from_millis(120));
        assert!(!fired.load(Ordering::SeqCst));
    }

    #[test]
    fn a_panicking_timer_job_does_not_stop_later_ones() {
        let timers = Timers::new();
        let (tx, rx) = channel();
        timers.add(Instant::now(), Box::new(|| panic!("timer panic")));
        timers.add(
            Instant::now() + Duration::from_millis(10),
            Box::new(move || tx.send(1).unwrap()),
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    }

    #[test]
    fn sending_after_the_receiver_is_gone_reports_closed() {
        let (tx, rx) = channel::<Input<u8>>();
        let sender = Sender::new(tx);
        assert!(sender.send(1).is_ok());
        drop(rx);
        assert_eq!(sender.send(2), Err(Closed));
        assert_eq!(Closed.to_string(), "the program has stopped");
    }
}
