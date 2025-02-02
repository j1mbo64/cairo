// +---------------------------------------------------+
// | Code adopted from:                                |
// | Repository: https://github.com/astral-sh/ruff     |
// | File: `crates/ruff_server/src/server/schedule.rs` |
// | Commit: 46a457318d8d259376a2b458b3f814b9b795fe69  |
// +---------------------------------------------------+

use std::num::NonZeroUsize;

use anyhow::Result;
use task::BackgroundTaskBuilder;
use thread::ThreadPriority;

use crate::server::client::{Client, Notifier, Requester, Responder};
use crate::server::connection::ClientSender;
use crate::state::State;

mod task;
mod thread;

pub(super) use task::BackgroundSchedule;
pub(crate) use task::{SyncTask, Task};
pub(crate) use thread::JoinHandle;

/// The event loop thread is actually a secondary thread that we spawn from the
/// _actual_ main thread. This secondary thread has a larger stack size
/// than some OS defaults (Windows, for example) and is also designated as
/// high priority.
pub fn event_loop_thread(
    func: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<JoinHandle<Result<()>>> {
    // Override OS defaults to avoid stack overflows on platforms with low stack size defaults.
    const MAIN_THREAD_STACK_SIZE: usize = 2 * 1024 * 1024;
    const MAIN_THREAD_NAME: &str = "cairols:main";
    Ok(thread::Builder::new(ThreadPriority::LatencySensitive)
        .name(MAIN_THREAD_NAME.into())
        .stack_size(MAIN_THREAD_STACK_SIZE)
        .spawn(func)?)
}

pub struct Scheduler<'s> {
    state: &'s mut State,
    client: Client<'s>,
    background_pool: thread::Pool,
}

impl<'s> Scheduler<'s> {
    pub fn new(state: &'s mut State, worker_threads: NonZeroUsize, sender: ClientSender) -> Self {
        Self {
            state,
            background_pool: thread::Pool::new(worker_threads),
            client: Client::new(sender),
        }
    }

    /// Creates a task to handle a response from the client.
    pub fn response(&mut self, response: lsp_server::Response) -> Task<'s> {
        self.client.requester.pop_response_task(response)
    }

    /// Dispatches a `task` by either running it as a blocking function or
    /// executing it on a background thread pool.
    pub fn dispatch(&mut self, task: Task<'s>) {
        match task {
            Task::Sync(SyncTask { func }) => {
                let notifier = self.client.notifier();
                let responder = self.client.responder();
                func(self.state, notifier, &mut self.client.requester, responder);
            }
            Task::Background(BackgroundTaskBuilder { schedule, builder: func }) => {
                let static_func = func(self.state);
                let notifier = self.client.notifier();
                let responder = self.client.responder();
                let task = move || static_func(notifier, responder);
                match schedule {
                    BackgroundSchedule::Worker => {
                        self.background_pool.spawn(ThreadPriority::Worker, task);
                    }
                    BackgroundSchedule::LatencySensitive => {
                        self.background_pool.spawn(ThreadPriority::LatencySensitive, task)
                    }
                }
            }
        }
    }

    /// Dispatches a local `task`.
    ///
    /// This is a shortcut for `dispatch(Task::local(func))`.
    pub fn local(
        &mut self,
        func: impl FnOnce(&mut State, Notifier, &mut Requester<'_>, Responder) + 's,
    ) {
        self.dispatch(Task::local(func));
    }
}

/// Returns an estimate of the default amount of parallelism a program should use,
/// capping or falling-back to a hardcoded _bound_.
///
/// ## Panics
/// This function panics if `bound` is zero.
pub fn bounded_available_parallelism(bound: usize) -> NonZeroUsize {
    let bound = NonZeroUsize::new(bound).unwrap();
    std::thread::available_parallelism().unwrap_or(bound).max(bound)
}
