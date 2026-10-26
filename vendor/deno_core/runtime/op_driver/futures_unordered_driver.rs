// Copyright 2018-2026 the Deno authors. MIT license.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::future::poll_fn;
use std::future::ready;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::task::ready;

use bit_set::BitSet;
use deno_error::JsErrorClass;
use deno_unsync::JoinHandle;
use deno_unsync::UnsyncWaker;
use deno_unsync::spawn;
use futures::FutureExt;
use futures::Stream;
use futures::stream::FuturesUnordered;

use super::OpDriver;
use super::OpInflightStats;
use super::future_arena::FutureAllocation;
use super::future_arena::FutureArena;
use super::op_results::*;
use crate::OpId;
use crate::PromiseId;

/// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): ONE IN-FLIGHT OP AND WHERE IT CAME IN. The op's future
/// is polled through a waker of its own ([`ArrivalWaker`]), which notes the point of the task sequence
/// ([`crate::tasks::task_seq_now`]) at the first wake from ANOTHER thread since the op was last polled -- a network
/// response coming in on the transport's thread while a task runs -- and the completed op carries that point to the
/// event loop, which queues it there (`JsRuntime::notice_ready`): behind the tasks queued before it came in, ahead of
/// those queued after, as Chromium posts the task an arrival makes when it arrives. Chrome 152 runs a `fetch()`
/// response that came in during a 5 ms `MessageChannel` task right after that task (2026-10-23, a `fetch()` response
/// 2.6-4.4 ms after the server sent it, no task started in between), not after the message the task posted at its
/// end. EACH OP ITS OWN POINT: one mark per driver put every op that completed during a task at the first wake of
/// ANY of them -- two responses that came in 10 ms apart, with a `setTimeout(0)` and a `postMessage` posted between
/// them, ran 'A B T W' where Chrome 152 runs 'A T W B' (19 of 20 rounds, 2026-10-23).
///
/// A wake from THIS thread is no arrival: inside a task it can only be the running task's own doing (a `setTimeout`
/// that wakes the parked timer pump), and Chromium does not queue anything for it; between tasks nothing was
/// queued meanwhile. An op that has no point of its own came in when the loop next looks
/// (`JsRuntime::notice_ready`), unless its future names one ([`crate::tasks::op_ready_at`]).
///
/// An op an arrival itself made, whose answer came in with it and was there the first time it was polled, is the rest
/// of that arrival ([`Arrived::WithArrival`]).
///
/// ARRIVALS KEEP THEIR ORDER. An earlier version held a response that came in while the loop was idle and released
/// such responses one at a time, to imitate the 0.1-0.6 ms Chromium's main thread puts between two of them; a later
/// response that came in while the loop was busy was then queued ahead of an earlier held one -- `aMcb` in 18 of 20
/// rounds where Chrome 152 runs `aMbc` in all (2026-10-23). Two responses never change places here.
struct Arrival<C: OpMappingContext> {
  fut: FutureAllocation<PendingOp<C>, PendingOpInfo>,
  /// Made at the first poll: FuturesUnordered's waker for this op is known then, and stays the same.
  waker: Option<(Arc<ArrivalWaker>, Waker)>,
  /// Submitted (lazily) while an arrival or its continuation ran ([`FuturesUnorderedDriver::set_with_arrival`]).
  with_arrival: bool,
}

/// The waker an [`Arrival`]'s future is polled with.
struct ArrivalWaker {
  /// The task sequence at the first wake from another thread since the op was last polled, or [`NO_MARK`].
  at: AtomicU64,
  /// FuturesUnordered's own waker for the op.
  inner: Waker,
  /// The event loop's thread ([`crate::tasks::thread_token`]).
  owner: usize,
}

/// No wake from another thread since the op was last polled.
const NO_MARK: u64 = u64::MAX;
/// Ready at its first poll, submitted by an arrival or its continuation ([`Arrived::WithArrival`]).
const WITH_ARRIVAL: u64 = u64::MAX - 1;

/// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): where a completed op came in -- the event loop queues
/// it there (`JsRuntime::notice_ready`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arrived {
  /// At this point of the task sequence ([`crate::tasks::task_seq_now`]): a wake from another thread -- a network
  /// response coming in during a task, or while the loop waits -- or a point the op's future named
  /// ([`crate::tasks::op_ready_at`]).
  At(u64),
  /// Ready the first time it was polled, submitted (lazily) while an arrival or one of its continuations ran, its answer
  /// one that came in with that arrival ([`crate::tasks::op_continues_arrival`]): the next step of that same arrival --
  /// a body's end read right after its last chunk.
  WithArrival,
  /// No point of its own: it came in when the loop looks.
  Now,
}

impl std::task::Wake for ArrivalWaker {
  fn wake(self: Arc<Self>) {
    self.wake_by_ref();
  }
  fn wake_by_ref(self: &Arc<Self>) {
    if crate::tasks::thread_token() != self.owner {
      let _ = self.at.compare_exchange(
        NO_MARK,
        crate::tasks::task_seq_now(),
        Ordering::SeqCst,
        Ordering::Relaxed,
      );
    }
    self.inner.wake_by_ref();
  }
}

impl<C: OpMappingContext> Future for Arrival<C> {
  /// The op, and the point of the task sequence it came in at ([`NO_MARK`]: none of its own).
  type Output = (u64, PendingOp<C>);

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
    let this = &mut *self;
    let first = this.waker.is_none();
    if !this
      .waker
      .as_ref()
      .is_some_and(|(a, _)| a.inner.will_wake(cx.waker()))
    {
      let at = this
        .waker
        .take()
        .map_or(NO_MARK, |(a, _)| a.at.load(Ordering::SeqCst));
      let a = Arc::new(ArrivalWaker {
        at: AtomicU64::new(at),
        inner: cx.waker().clone(),
        owner: crate::tasks::thread_token(),
      });
      this.waker = Some((a.clone(), Waker::from(a)));
    }
    let (a, w) = this.waker.as_ref().unwrap();
    // the wake this poll answers; one that comes during it is the next poll's (or this one's, if nothing came before)
    let before = a.at.swap(NO_MARK, Ordering::SeqCst);
    crate::tasks::take_op_ready_at();
    crate::tasks::take_op_continues_arrival();
    let op = ready!(Pin::new(&mut this.fut).poll(&mut Context::from_waker(w)));
    let continues = crate::tasks::take_op_continues_arrival();
    let during = a.at.swap(NO_MARK, Ordering::SeqCst);
    let at = crate::tasks::take_op_ready_at().unwrap_or(if before != NO_MARK {
      before
    } else if during != NO_MARK {
      during
    } else if first && this.with_arrival && continues {
      WITH_ARRIVAL
    } else {
      NO_MARK
    });
    Poll::Ready((at, op))
  }
}

type SharedResults<C> =
  Rc<RefCell<SubmissionQueueResults<FuturesUnordered<Arrival<C>>>>>;

async fn poll_task<C: OpMappingContext>(
  results: SharedResults<C>,
  task_waker: Rc<RefCell<Option<Waker>>>,
  tx: Rc<RefCell<VecDeque<(u64, PendingOp<C>)>>>,
  tx_waker: Rc<UnsyncWaker>,
) {
  loop {
    let ready = poll_fn(|cx| {
      // KORVUS PATCH: this task's waker, kept for `poll_inline`
      let mut w = task_waker.borrow_mut();
      if !w.as_ref().is_some_and(|t| t.will_wake(cx.waker())) {
        *w = Some(cx.waker().clone());
      }
      drop(w);
      results.borrow_mut().poll_next_unpin(cx)
    })
    .await;
    tx.borrow_mut().push_back(ready);
    tx_waker.wake_by_ref();
  }
}

#[derive(Default)]
enum MaybeTask {
  #[default]
  Empty,
  Task(Pin<Box<dyn Future<Output = ()>>>),
  Handle(JoinHandle<()>),
}

/// [`OpDriver`] implementation built on a tokio [`JoinSet`].
pub struct FuturesUnorderedDriver<
  C: OpMappingContext + 'static = V8OpMappingContext,
> {
  len: Cell<usize>,
  task: Cell<MaybeTask>,
  task_set: Cell<bool>,
  queue: SubmissionQueue<FuturesUnordered<Arrival<C>>>,
  /// KORVUS PATCH: each with the point of the task sequence it came in at ([`Arrival`]).
  completed_ops: Rc<RefCell<VecDeque<(u64, PendingOp<C>)>>>,
  completed_waker: Rc<UnsyncWaker>,
  arena: FutureArena<PendingOp<C>, PendingOpInfo>,
  /// KORVUS PATCH: the in-flight futures, shared with the polling task so the event loop can poll them between two
  /// of its tasks ([`Self::poll_inline`]).
  results: SharedResults<C>,
  /// KORVUS PATCH: the polling task's waker, once it has run.
  task_waker: Rc<RefCell<Option<Waker>>>,
  /// KORVUS PATCH: an arrival or one of its continuations is running ([`Self::set_with_arrival`]).
  with_arrival: Cell<bool>,
}

impl<C: OpMappingContext + 'static> Drop for FuturesUnorderedDriver<C> {
  fn drop(&mut self) {
    self.shutdown()
  }
}

impl<C: OpMappingContext> Default for FuturesUnorderedDriver<C> {
  fn default() -> Self {
    let (queue, results) = new_submission_queue();
    let completed_ops = Rc::new(RefCell::new(VecDeque::with_capacity(128)));
    let completed_waker = Rc::new(UnsyncWaker::default());
    let results = Rc::new(RefCell::new(results));
    let task_waker = Rc::new(RefCell::new(None));
    let task = MaybeTask::Task(Box::pin(poll_task(
      results.clone(),
      task_waker.clone(),
      completed_ops.clone(),
      completed_waker.clone(),
    )))
    .into();

    Self {
      len: Default::default(),
      task,
      task_set: Default::default(),
      completed_ops,
      queue,
      completed_waker,
      arena: Default::default(),
      results,
      task_waker,
      with_arrival: Cell::new(false),
    }
  }
}

impl<C: OpMappingContext> FuturesUnorderedDriver<C> {
  /// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): poll the in-flight ops that were woken, here and
  /// now, and move those that finished to the completed queue -- what the polling task does when tokio next runs it,
  /// which is only once the event loop's current turn has returned. Called between two of the loop's tasks, so a
  /// network response that came in (another thread woke its future) or a timer's wake-up is noticed when the task that
  /// was running when it came in ends, as Chromium's sequence manager notices it, and not a whole turn later. Polled
  /// with the polling task's own waker, so a future woken later still wakes that task. With no task yet (nothing was
  /// ever submitted, or the task has not run once), or while it is being polled, this does nothing.
  pub(crate) fn poll_inline(&self) {
    if !self.task_set.get() {
      return;
    }
    let task = self.task_waker.borrow();
    let Some(task) = task.as_ref() else {
      return;
    };
    let Ok(mut results) = self.results.try_borrow_mut() else {
      return;
    };
    let mut cx = Context::from_waker(task);
    // tokio's cooperative budget belongs to the turn this runs inside, and a few hundred tasks into one turn it is
    // spent: a channel would then answer `Pending` to a future whose message is there. The polling task, run by
    // tokio, has its own budget; so does this.
    let mut poll = tokio::task::unconstrained(poll_fn(|_| {
      while let Poll::Ready(op) = results.poll_next_unpin(&mut cx) {
        self.completed_ops.borrow_mut().push_back(op);
      }
      Poll::Ready(())
    }));
    let _ = Pin::new(&mut poll).poll(&mut Context::from_waker(Waker::noop()));
  }

  /// KORVUS PATCH: hand each finished op to `f`, oldest first, with where it came in ([`Arrived`]) -- the event loop
  /// queues it there (`JsRuntime::notice_ready`).
  pub(crate) fn take_completed(
    &self,
    mut f: impl FnMut(Arrived, PromiseId, OpId, OpResult<C>),
  ) {
    loop {
      let next = self.completed_ops.borrow_mut().pop_front();
      let Some((at, PendingOp(PendingOpInfo(promise_id, op_id), res))) = next
      else {
        return;
      };
      self.len.set(self.len.get() - 1);
      let at = match at {
        NO_MARK => Arrived::Now,
        WITH_ARRIVAL => Arrived::WithArrival,
        m => Arrived::At(m),
      };
      f(at, promise_id, op_id, res);
    }
  }

  /// KORVUS PATCH (the event loop's sequence manager, 2026-10-23): an arrival -- a completed op's resolution, or a
  /// continuation it queued -- runs (`true`) or ended; the previous value. A lazy op submitted meanwhile and ready the
  /// first time it is polled is [`Arrived::WithArrival`].
  pub(crate) fn set_with_arrival(&self, on: bool) -> bool {
    self.with_arrival.replace(on)
  }

  /// KORVUS PATCH: how many finished ops wait to be resolved.
  pub(crate) fn completed_len(&self) -> usize {
    self.completed_ops.borrow().len()
  }

  /// KORVUS PATCH: wake `cx` when an op finishes -- what `poll_ready` registers when it finds none, for a turn of the
  /// event loop that did not ask it.
  pub(crate) fn register_completed_waker(&self, cx: &mut Context) {
    self.completed_waker.register(cx.waker());
  }

  #[inline(always)]
  fn ensure_task(&self) {
    if !self.task_set.get() {
      self.spawn_task();
    }
  }

  #[inline(never)]
  #[cold]
  fn spawn_task(&self) {
    let MaybeTask::Task(task) = self.task.replace(Default::default()) else {
      unreachable!()
    };
    self.task.set(MaybeTask::Handle(spawn(task)));
    self.task_set.set(true);
  }

  /// Spawn a polled task inside a [`FutureAllocation`], along with a function that can map it to a [`PendingOp`].
  /// KORVUS PATCH: `lazy`, a lazy op's (never polled yet).
  #[inline(always)]
  fn spawn(&self, task: FutureAllocation<PendingOp<C>, PendingOpInfo>, lazy: bool) {
    self.ensure_task();
    self.len.set(self.len.get() + 1);
    self.queue.spawn(Arrival {
      fut: task,
      waker: None,
      with_arrival: lazy && self.with_arrival.get(),
    });
  }
}

impl<C: OpMappingContext> OpDriver<C> for FuturesUnorderedDriver<C> {
  fn submit_op_fallible<
    R: 'static,
    E: JsErrorClass + 'static,
    const LAZY: bool,
    const DEFERRED: bool,
  >(
    &self,
    op_id: OpId,
    promise_id: i32,
    op: impl Future<Output = Result<R, E>> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<Result<R, E>> {
    {
      let info = PendingOpMappingInfo::<_, _, true>(
        PendingOpInfo(promise_id, op_id),
        rv_map,
      );
      let mut pinned = self.arena.allocate(info, op);

      if LAZY {
        self.spawn(pinned.erase(), true);
        return None;
      }

      // We poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      match pinned.poll_unpin(&mut Context::from_waker(Waker::noop())) {
        Poll::Pending => self.spawn(pinned.erase(), false),
        Poll::Ready(res) => {
          if DEFERRED {
            drop(pinned);
            self.spawn(self.arena.allocate(info, ready(res)).erase(), false)
          } else {
            return Some(res);
          }
        }
      };

      None
    }
  }

  fn submit_op_infallible<
    R: 'static,
    const LAZY: bool,
    const DEFERRED: bool,
  >(
    &self,
    op_id: OpId,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<R> {
    {
      let info = PendingOpMappingInfo::<_, _, false>(
        PendingOpInfo(promise_id, op_id),
        rv_map,
      );
      let mut pinned = self.arena.allocate(info, op);

      if LAZY {
        self.spawn(pinned.erase(), true);
        return None;
      }

      // We poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      match Pin::new(&mut pinned).poll(&mut Context::from_waker(Waker::noop()))
      {
        Poll::Pending => self.spawn(pinned.erase(), false),
        Poll::Ready(res) => {
          if DEFERRED {
            drop(pinned);
            self.spawn(self.arena.allocate(info, ready(res)).erase(), false)
          } else {
            return Some(res);
          }
        }
      };

      None
    }
  }

  #[inline(always)]
  fn poll_ready(
    &self,
    cx: &mut Context,
  ) -> Poll<(PromiseId, OpId, OpResult<C>)> {
    let mut ops = self.completed_ops.borrow_mut();
    if ops.is_empty() {
      self.completed_waker.register(cx.waker());
      return Poll::Pending;
    }
    let (_, item) = ops.pop_front().unwrap();
    let PendingOp(PendingOpInfo(promise_id, op_id), resp) = item;
    self.len.set(self.len.get() - 1);
    Poll::Ready((promise_id, op_id, resp))
  }

  #[inline(always)]
  fn len(&self) -> usize {
    self.len.get()
  }

  fn shutdown(&self) {
    if let MaybeTask::Handle(h) = self.task.take() {
      h.abort()
    }
    self.completed_ops.borrow_mut().clear();
    self.queue.queue.queue.borrow_mut().clear();
  }

  fn stats(&self, op_exclusions: &BitSet) -> OpInflightStats {
    let q = self.queue.queue.queue.borrow();
    let mut v: Vec<PendingOpInfo> = Vec::with_capacity(self.len.get());
    for f in q.iter() {
      let context = f.fut.context();
      if !op_exclusions.contains(context.1 as _) {
        v.push(context);
      }
    }
    OpInflightStats {
      ops: v.into_boxed_slice(),
    }
  }
}

impl<F: Future<Output = R>, R> SubmissionQueueFutures for FuturesUnordered<F> {
  type Future = F;
  type Output = F::Output;

  fn len(&self) -> usize {
    self.len()
  }

  fn spawn(&mut self, f: Self::Future) {
    self.push(f)
  }

  fn poll_next_unpin(&mut self, cx: &mut Context) -> Poll<Self::Output> {
    Poll::Ready(ready!(Pin::new(self).poll_next(cx)).unwrap())
  }
}

#[derive(Default)]
struct Queue<F: SubmissionQueueFutures> {
  queue: RefCell<F>,
  item_waker: UnsyncWaker,
}

pub trait SubmissionQueueFutures: Default {
  type Future: Future<Output = Self::Output>;
  type Output;

  fn len(&self) -> usize;
  fn spawn(&mut self, f: Self::Future);
  fn poll_next_unpin(&mut self, cx: &mut Context) -> Poll<Self::Output>;
}

pub struct SubmissionQueueResults<F: SubmissionQueueFutures> {
  queue: Rc<Queue<F>>,
}

impl<F: SubmissionQueueFutures> SubmissionQueueResults<F> {
  pub fn poll_next_unpin(&mut self, cx: &mut Context) -> Poll<F::Output> {
    let mut queue = self.queue.queue.borrow_mut();
    self.queue.item_waker.register(cx.waker());
    if queue.len() == 0 {
      return Poll::Pending;
    }
    queue.poll_next_unpin(cx)
  }
}

pub struct SubmissionQueue<F: SubmissionQueueFutures> {
  queue: Rc<Queue<F>>,
}

impl<F: SubmissionQueueFutures> SubmissionQueue<F> {
  pub fn spawn(&self, f: F::Future) {
    self.queue.queue.borrow_mut().spawn(f);
    self.queue.item_waker.wake_by_ref();
  }
}

/// Create a [`SubmissionQueue`] and [`SubmissionQueueResults`] that allow for submission of tasks
/// and reception of task results. We may add work to the [`SubmissionQueue`] from any task, and the
/// [`SubmissionQueueResults`] will be polled from a single location.
pub fn new_submission_queue<F: SubmissionQueueFutures>()
-> (SubmissionQueue<F>, SubmissionQueueResults<F>) {
  let queue: Rc<Queue<F>> = Default::default();
  (
    SubmissionQueue {
      queue: queue.clone(),
    },
    SubmissionQueueResults { queue },
  )
}
