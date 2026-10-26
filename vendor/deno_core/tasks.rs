// Copyright 2018-2026 the Deno authors. MIT license.

use std::marker::PhantomData;
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use futures::task::AtomicWaker;

pub(crate) type UnsendTask = Box<dyn FnOnce(&mut v8::PinScope) + 'static>;
type SendTask = Box<dyn FnOnce(&mut v8::PinScope) + Send + 'static>;

/// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): every task queued through a spawner, in any runtime,
/// takes the next number of this one sequence -- Chromium's enqueue order. An async op that comes in reads it
/// ([`task_seq_now`]: a network response's wake-up from another thread, a timer the timer task source moved at a
/// selection, [`op_ready_at`]), and the event loop puts the op behind the tasks queued before it and AHEAD of those
/// queued after it (`JsRuntime::notice_ready`). One sequence for every runtime: it only has to grow.
static TASK_SEQ: AtomicU64 = AtomicU64::new(0);

/// The number the next task queued will take: whatever comes in now came in after every task numbered below it.
pub fn task_seq_now() -> u64 {
  TASK_SEQ.load(Ordering::SeqCst)
}

thread_local! {
  /// [`op_ready_at`]'s point, for the op whose future is being polled.
  static OP_READY_AT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
  /// [`op_continues_arrival`], for the op whose future is being polled.
  static OP_CONTINUES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
  /// One byte per thread, its address the thread's token ([`thread_token`]).
  static THREAD_TOKEN: u8 = const { 0 };
}

/// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): called by an async op's future in the poll that
/// completes it, the op CAME IN at `seq` of the task sequence (a value [`task_seq_now`] answered): the event loop queues
/// it behind the tasks numbered below `seq` and ahead of the rest, wherever it was noticed. The timer task source names
/// the selection at which a timer came due: Chromium moves every delayed task that is due to its work queue at one
/// selection, so timers due together run back to back, not each behind what the one before it posted.
pub fn op_ready_at(seq: u64) {
  let _ = OP_READY_AT.try_with(|c| c.set(Some(seq)));
}

/// Take [`op_ready_at`]'s point (and clear it, before an op's future is polled).
pub(crate) fn take_op_ready_at() -> Option<u64> {
  OP_READY_AT.try_with(|c| c.take()).ok().flatten()
}

/// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): called by an async op's future in the poll that
/// completes it, its answer came in WITH the arrival whose resolution submitted it -- the end of a response's body,
/// which comes in with its last bytes. Ready the first time it is polled, submitted (lazily) while that arrival or one
/// of its continuations ran, the event loop runs it as that arrival's rest, at the front. An op that merely finds its
/// answer waiting -- the next message of a channel, posted after something else came in -- does not say so, and is
/// queued when the loop looks.
pub fn op_continues_arrival() {
  let _ = OP_CONTINUES.try_with(|c| c.set(true));
}

/// Take [`op_continues_arrival`]'s word (and clear it, before an op's future is polled).
pub(crate) fn take_op_continues_arrival() -> bool {
  OP_CONTINUES.try_with(|c| c.replace(false)).unwrap_or(false)
}

/// This thread, as a number no other live thread has (0 once its thread-locals are gone): a wake from a thread other
/// than the event loop's is an arrival (`ArrivalWaker`).
pub(crate) fn thread_token() -> usize {
  THREAD_TOKEN
    .try_with(|t| t as *const u8 as usize)
    .unwrap_or(0)
}

static_assertions::assert_not_impl_any!(V8TaskSpawnerFactory: Send);
static_assertions::assert_not_impl_any!(V8TaskSpawner: Send);
static_assertions::assert_impl_all!(V8CrossThreadTaskSpawner: Send);

/// The [`V8TaskSpawnerFactory`] must be created on the same thread as the thread that runs tasks.
///
/// This factory is not [`Send`] because it may contain `!Send` tasks submitted by a
/// [`V8CrossThreadTaskSpawner`]. It is only safe to send this object to another thread if you plan on
/// submitting [`Send`] tasks to it, which is what [`V8CrossThreadTaskSpawner`] does.
#[derive(Default)]
pub(crate) struct V8TaskSpawnerFactory {
  // TODO(mmastrac): ideally we wouldn't box if we could use arena allocation and a max submission size
  // TODO(mmastrac): we may want to split the Send and !Send tasks
  /// The set of tasks, non-empty if `has_tasks` is set -- KORVUS PATCH: each with its [`TASK_SEQ`] number.
  tasks: Mutex<Vec<(u64, SendTask)>>,
  /// A flag we can poll without any locks.
  has_tasks: AtomicBool,
  /// The polled waker, woken on task submission.
  waker: AtomicWaker,
  /// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): set while the event loop resolves the async ops
  /// that completed -- an ARRIVAL, a network response or a timer's wake-up coming in -- so that the continuation
  /// tasks those resolutions queue ([`V8TaskSpawner::spawn_continuation`]) are the arrival's own task and run next,
  /// instead of behind every task queued meanwhile. See `JsRuntime::dispatch_task_spawner`.
  arrival: AtomicBool,
  /// The continuations queued during the arrival now being resolved, in their order.
  continuations: Mutex<Vec<SendTask>>,
  /// Mark as `!Send`. See note above.
  _unsend_marker: PhantomData<*const ()>,
}

impl V8TaskSpawnerFactory {
  pub fn new_same_thread_spawner(self: Arc<Self>) -> V8TaskSpawner {
    V8TaskSpawner {
      tasks: self,
      _unsend_marker: PhantomData,
    }
  }

  pub fn new_cross_thread_spawner(self: Arc<Self>) -> V8CrossThreadTaskSpawner {
    V8CrossThreadTaskSpawner { tasks: self }
  }

  /// `false` guarantees that there are no queued tasks, while `true` means that it is likely (but not guaranteed)
  /// that tasks exist.
  ///
  /// Calls should prefer using the waker, but this is left while we rework the event loop.
  pub fn has_pending_tasks(&self) -> bool {
    // Ensure that all reads after this point happen-after we load from the atomic
    self.has_tasks.load(Ordering::Acquire)
  }

  /// Poll this set of tasks, returning a non-empty set of tasks if there have
  /// been any queued, or registering the waker if not.
  pub fn poll_inner(&self, cx: &mut Context) -> Poll<Vec<(u64, UnsendTask)>> {
    // Check the flag first -- if it's false we definitely have no tasks. AcqRel semantics ensure
    // that this read happens-before the vector read below, and that any writes happen-after the success
    // case.
    if self
      .has_tasks
      .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
      .is_err()
    {
      self.waker.register(cx.waker());
      return Poll::Pending;
    }

    let mut lock = self.tasks.lock().unwrap();
    let tasks = std::mem::take(lock.deref_mut());
    if tasks.is_empty() {
      // Unlikely race lost -- the task submission to the queue and flag are not atomic, so it's
      // possible we ended up with an extra poll here. This only shows up under Miri, but as it is
      // possible we do need to handle it.
      self.waker.register(cx.waker());
      return Poll::Pending;
    }

    // SAFETY: we are removing the Send trait as we return the tasks here to prevent
    // these tasks from accidentally leaking to another thread.
    let tasks = unsafe {
      std::mem::transmute::<Vec<(u64, SendTask)>, Vec<(u64, UnsendTask)>>(
        tasks,
      )
    };
    Poll::Ready(tasks)
  }

  /// KORVUS PATCH: the event loop starts resolving completed ops ([`Self::arrival`]).
  pub(crate) fn begin_arrival(&self) {
    self.arrival.store(true, Ordering::Relaxed);
  }

  /// KORVUS PATCH: the ops are resolved; the continuations they queued, in their order, to run next.
  pub(crate) fn end_arrival(&self) -> Vec<UnsendTask> {
    self.arrival.store(false, Ordering::Relaxed);
    let tasks = std::mem::take(self.continuations.lock().unwrap().deref_mut());
    // SAFETY: as in `poll_inner`: these never leave the runtime's thread (only `V8TaskSpawner`, which is
    // `!Send`, queues continuations).
    unsafe { std::mem::transmute::<Vec<SendTask>, Vec<UnsendTask>>(tasks) }
  }

  fn spawn_continuation(&self, task: SendTask) {
    if self.arrival.load(Ordering::Relaxed) {
      self.continuations.lock().unwrap().push(task);
    } else {
      self.spawn(task);
    }
  }

  fn spawn(&self, task: SendTask) {
    {
      // numbered under the lock, so the set is in number order
      let mut tasks = self.tasks.lock().unwrap();
      let seq = TASK_SEQ.fetch_add(1, Ordering::SeqCst);
      tasks.push((seq, task));
    }
    // TODO(mmastrac): can we skip the mutex here?
    // Release ordering means that the writes in the above lock happen-before the atomic store
    self.has_tasks.store(true, Ordering::Release);
    self.waker.wake();
  }
}

/// Allows for submission of v8 tasks on the same thread.
#[derive(Clone)]
pub struct V8TaskSpawner {
  // TODO(mmastrac): can we split the waker into a send and !send one?
  tasks: Arc<V8TaskSpawnerFactory>,
  _unsend_marker: PhantomData<*const ()>,
}

impl V8TaskSpawner {
  /// Spawn a task that runs within the [`crate::JsRuntime`] event loop from the same thread
  /// that the runtime is running on. This function is re-entrant-safe and may be called from
  /// ops, from outside of a [`v8::HandleScope`] in a plain `async`` task, or even from within
  /// another, previously-spawned task.
  ///
  /// The task is handed off to be run the next time the event loop is polled, and there are
  /// no guarantees as to when this may happen.
  ///
  /// # Safety
  ///
  /// The task shares the same [`v8::HandleScope`] as the core event loop, which means that it
  /// must maintain the scope in a valid state to avoid corrupting or destroying the runtime.
  ///
  /// For example, if the code called by this task can raise an exception, the task must ensure
  /// that it calls that code within a new [`v8::TryCatch`] to avoid the exception leaking to the
  /// event loop's [`v8::HandleScope`].
  pub fn spawn<F>(&self, f: F)
  where
    F: FnOnce(&mut v8::PinScope<'_, '_>) + 'static,
  {
    let task: Box<dyn FnOnce(&mut v8::PinScope<'_, '_>)> = Box::new(f);
    // SAFETY: we are transmuting Send into a !Send handle but we guarantee this object will never
    // leave the current thread because `V8TaskSpawner` is !Send.
    let task: Box<dyn FnOnce(&mut v8::PinScope<'_, '_>) + Send> =
      unsafe { std::mem::transmute(task) };
    self.tasks.spawn(task)
  }

  /// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): [`Self::spawn`] for the task that CONTINUES an
  /// async op's result -- a promise continuation the embedder runs as a task of its own. Queued while the event loop
  /// is resolving completed ops, it is that arrival's task and runs right after the resolution, ahead of the tasks
  /// already queued (Chromium's sequence manager runs a network response's or a timer's task in the order it came
  /// in, not after everything posted while it was being handed over). Queued at any other time it is [`Self::spawn`].
  pub fn spawn_continuation<F>(&self, f: F)
  where
    F: FnOnce(&mut v8::PinScope<'_, '_>) + 'static,
  {
    let task: Box<dyn FnOnce(&mut v8::PinScope<'_, '_>)> = Box::new(f);
    // SAFETY: as in `spawn`.
    let task: Box<dyn FnOnce(&mut v8::PinScope<'_, '_>) + Send> =
      unsafe { std::mem::transmute(task) };
    self.tasks.spawn_continuation(task)
  }
}

/// Allows for submission of v8 tasks on any thread.
#[derive(Clone)]
pub struct V8CrossThreadTaskSpawner {
  tasks: Arc<V8TaskSpawnerFactory>,
}

// SAFETY: the underlying V8TaskSpawnerFactory is not Send, but we always submit Send tasks
// to it from this spawner.
unsafe impl Send for V8CrossThreadTaskSpawner {}

impl V8CrossThreadTaskSpawner {
  /// Spawn a task that runs within the [`crate::JsRuntime`] event loop, potentially (but not
  /// required to be) from a different thread than the runtime is running on.
  ///
  /// The task is handed off to be run the next time the event loop is polled, and there are
  /// no guarantees as to when this may happen.
  ///
  /// # Safety
  ///
  /// The task shares the same [`v8::HandleScope`] as the core event loop, which means that it
  /// must maintain the scope in a valid state to avoid corrupting or destroying the runtime.
  ///
  /// For example, if the code called by this task can raise an exception, the task must ensure
  /// that it calls that code within a new [`v8::TryCatch`] to avoid the exception leaking to the
  /// event loop's [`v8::HandleScope`].
  pub fn spawn<F>(&self, f: F)
  where
    F: FnOnce(&mut v8::PinScope<'_, '_>) + Send + 'static,
  {
    self.tasks.spawn(Box::new(f))
  }

  /// Spawn a task that runs within the [`crate::JsRuntime`] event loop from a different thread
  /// than the runtime is running on.
  ///
  /// This function will deadlock if called from the same thread as the [`crate::JsRuntime`], and
  /// there are no checks for this case.
  ///
  /// As this function blocks until the task has run to completion (or panics/deadlocks), it is
  /// safe to borrow data from the local environment and use it within the closure.
  ///
  /// The task is handed off to be run the next time the event loop is polled, and there are
  /// no guarantees as to when this may happen, however the function will not return until the
  /// task has been fully run to completion.
  ///
  /// # Safety
  ///
  /// The task shares the same [`v8::HandleScope`] as the core event loop, which means that it
  /// must maintain the scope in a valid state to avoid corrupting or destroying the runtime.
  ///
  /// For example, if the code called by this task can raise an exception, the task must ensure
  /// that it calls that code within a new [`v8::TryCatch`] to avoid the exception leaking to the
  /// event loop's [`v8::HandleScope`].
  pub fn spawn_blocking<'a, F, T>(&self, f: F) -> T
  where
    F: FnOnce(&mut v8::PinScope) -> T + Send + 'a,
    T: Send + 'a,
  {
    let (tx, rx) = std::sync::mpsc::sync_channel(0);
    let task: Box<dyn FnOnce(&mut v8::PinScope<'_, '_>) + Send> =
      Box::new(|scope| {
        let r = f(scope);
        _ = tx.send(r);
      });
    // SAFETY: We can safely transmute to the 'static lifetime because we guarantee this method will either
    // complete fully by the time it returns, deadlock or panic.
    let task: SendTask = unsafe { std::mem::transmute(task) };
    self.tasks.spawn(task);
    rx.recv().unwrap()
  }
}

#[cfg(test)]
mod tests {
  use std::future::poll_fn;

  use tokio::task::LocalSet;

  use super::*;

  // https://github.com/tokio-rs/tokio/issues/6155
  #[test]
  #[cfg(not(all(miri, target_os = "linux")))]
  fn test_spawner_serial() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(1)
      .build()
      .unwrap();
    runtime.block_on(async {
      let factory = Arc::<V8TaskSpawnerFactory>::default();
      let cross_thread_spawner = factory.clone().new_cross_thread_spawner();
      let local_set = LocalSet::new();

      const COUNT: usize = 1000;

      let task = runtime.spawn(async move {
        for _ in 0..COUNT {
          cross_thread_spawner.spawn(|_| {});
        }
      });

      local_set.spawn_local(async move {
        let mut count = 0;
        loop {
          count += poll_fn(|cx| factory.poll_inner(cx)).await.len();
          if count >= COUNT {
            break;
          }
        }
      });

      local_set.await;
      _ = task.await;
    });
  }

  // https://github.com/tokio-rs/tokio/issues/6155
  #[test]
  #[cfg(not(all(miri, target_os = "linux")))]
  fn test_spawner_parallel() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(1)
      .build()
      .unwrap();
    runtime.block_on(async {
      let factory = Arc::<V8TaskSpawnerFactory>::default();
      let cross_thread_spawner = factory.clone().new_cross_thread_spawner();
      let local_set = LocalSet::new();

      const COUNT: usize = 100;
      let mut tasks = vec![];
      for _ in 0..COUNT {
        let cross_thread_spawner = cross_thread_spawner.clone();
        tasks.push(runtime.spawn(async move {
          cross_thread_spawner.spawn(|_| {});
        }));
      }

      local_set.spawn_local(async move {
        let mut count = 0;
        loop {
          count += poll_fn(|cx| factory.poll_inner(cx)).await.len();
          if count >= COUNT {
            break;
          }
        }
      });

      local_set.await;
      for task in tasks.drain(..) {
        _ = task.await;
      }
    });
  }
}
