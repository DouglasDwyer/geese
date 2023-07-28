use crate::*;
use bitvec::prelude::*;
use bitvec::ptr::*;
use smallvec::*;
use std::collections::vec_deque::*;
use vecdeque_stableix::*;

/// Organizes the receipt and delegation of event handling.
pub(crate) struct EventManager {
    /// The index of the nearest blocking event that requires the main thread to execute it.
    blocked_main_event: isize,
    /// An event that has stalled the event pipeline, if any.
    clogged_event: Option<Box<dyn Any + Send + Sync>>,
    /// A stack of event queues, which holds events that have not yet entered the processing pipeline.
    event_lists: SmallVec<[VecDeque<Box<dyn Any + Send + Sync>>; Self::DEFAULT_EVENT_BUFFER_SIZE]>,
    /// The receiver that collates new events.
    receiver: EventReceiver,
    /// The queue of event nodes that are undergoing processing.
    in_progress: Deque<EventNode, isize>,
    /// A rolling list of systems that are dependents or dependencies of in-progress event nodes.
    working_transitive_dependencies_bi: BitVec,
    /// A rolling list of systems that are mutably borrowed by in-progress event nodes.
    working_transitive_dependencies_mut: BitVec,
}

impl EventManager {
    /// The default amount of space to allocate for objects in an event queue.
    const DEFAULT_EVENT_BUFFER_SIZE: usize = 4;

    /// Creates a new, empty event manager and an associated sender for raising events.
    #[inline(always)]
    pub fn new() -> (Self, std::sync::mpsc::Sender<Event>) {
        let (receiver, sender) = EventReceiver::new();

        (
            Self {
                blocked_main_event: isize::MAX,
                clogged_event: None,
                event_lists: SmallVec::new(),
                receiver,
                in_progress: Deque::new(),
                working_transitive_dependencies_bi: BitVec::new(),
                working_transitive_dependencies_mut: BitVec::new(),
            },
            sender,
        )
    }

    /// Configures this event manager for interaction with the given inner context.
    #[inline(always)]
    pub fn configure(&mut self, ctx: &ContextInner) {
        debug_assert!(
            self.in_progress.is_empty(),
            "In progress queue was not empty when configuring event manager."
        );

        *self.in_progress.counter_mut() = isize::MIN;
        self.working_transitive_dependencies_bi
            .resize(ctx.systems.len(), false);
        self.working_transitive_dependencies_mut
            .resize(ctx.systems.len(), false);
    }

    /// Updates the state of this event manager and returns the reason that the event manager stopped processing.
    #[inline(always)]
    pub fn update_state(&mut self) -> EventManagerState {
        unsafe {
            if self.clogged_event.is_some() {
                EventManagerState::CycleClogged(take(&mut self.clogged_event).unwrap_unchecked())
            } else {
                debug_assert!(self.event_lists.is_empty());
                EventManagerState::Complete()
            }
        }
    }

    /// Pushes all externally-raised events onto the top event queue.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, the manager must have at least one event cycle active.
    #[inline(always)]
    pub unsafe fn gather_external_events(&mut self) {
        self.event_lists
            .last_mut()
            .unwrap_unchecked()
            .extend(self.receiver.get_all().collect::<VecDeque<_>>());
    }

    /// Begins a new externally-driven event cycle.
    #[inline(always)]
    pub fn push_external_event_cycle(&mut self) {
        debug_assert!(
            self.in_progress.is_empty(),
            "In progress queue was not empty when starting a new event cycle."
        );

        self.event_lists.push(VecDeque::new());
    }

    /// Pushes a new event cycle onto the pipeline stack with the given list of events.
    #[inline(always)]
    pub fn push_event_cycle(&mut self, events: impl Iterator<Item = Box<dyn Any + Send + Sync>>) {
        unsafe {
            debug_assert!(
                self.in_progress.is_empty(),
                "In progress queue was not empty when starting a new event cycle."
            );

            self.event_lists.push(VecDeque::new());
            self.event_lists
                .last_mut()
                .unwrap_unchecked()
                .extend(events);
        }
    }

    /// Attempts to select a new job from this pipeline.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, this event manager must have been configured with the provided
    /// inner context. The inner context may not have changed since.
    #[inline(always)]
    pub unsafe fn next_job(&mut self, ctx: &ContextInner) -> Result<EventJob, EventJobError> {
        let main_thread = std::thread::current().id() == ctx.owning_thread;

        if self.blocked_main_event < isize::MAX {
            if main_thread {
                let mut node = self
                    .in_progress
                    .get_mut(self.blocked_main_event)
                    .unwrap_unchecked();
                node.state = EventState::Processing;
                return Ok(EventJob {
                    event: node.event.as_ref().unwrap_unchecked().clone(),
                    handler: node.handler,
                    id: std::mem::replace(&mut self.blocked_main_event, isize::MAX),
                });
            } else {
                return Err(EventJobError::MainThreadRequired);
            }
        }

        self.working_transitive_dependencies_bi.fill(false);
        self.working_transitive_dependencies_mut.fill(false);

        if let Some(result) = self.get_queued_job(ctx, main_thread) {
            self.blocked_main_event = isize::MAX;
            return Ok(result);
        }

        if self.clogged_event.is_none() {
            if let Some(result) = self.queue_new_jobs(ctx, main_thread) {
                self.blocked_main_event = isize::MAX;
                return Ok(result);
            }
        }

        if self.in_progress.is_empty() {
            Err(EventJobError::Complete)
        } else if self.blocked_main_event < isize::MAX {
            Err(EventJobError::MainThreadRequired)
        } else {
            Err(EventJobError::Busy)
        }
    }

    /// Marks the given job as complete.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, the job must correspond to a valid event node
    /// in this manager's event pipeline.
    #[inline(always)]
    pub unsafe fn complete_job(&mut self, job: &EventJob) {
        let front_ev = self.in_progress.front().unwrap_unchecked().event.clone();
        let value = self.in_progress.get_mut(job.id).unwrap_unchecked();
        value.state = EventState::Complete;

        let emitted = self.receiver.get();
        if transmute::<_, &usize>(&value.event) == transmute::<_, &usize>(&front_ev) {
            self.event_lists
                .last_mut()
                .unwrap_unchecked()
                .extend(emitted);

            if job.id == *self.in_progress.counter() {
                self.drop_front();
            }
        } else {
            value.emitted_events.extend(emitted);
        }
    }

    /// Attempts to select a job from the list of available event nodes in the event pipeline.
    #[inline(always)]
    unsafe fn get_queued_job(&mut self, ctx: &ContextInner, main_thread: bool) -> Option<EventJob> {
        let start = *self.in_progress.counter();
        for (id, node) in self
            .in_progress
            .iter_mut()
            .filter(|(_, event)| event.state != EventState::Complete)
        {
            if let Some(value) = Self::try_select_job(JobSelectionParams {
                ctx,
                id,
                start,
                node,
                main_thread,
                working_transitive_dependencies_bi: &mut self.working_transitive_dependencies_bi,
                working_transitive_dependencies_mut: &mut self.working_transitive_dependencies_mut,
                main_thread_required: &mut self.blocked_main_event,
            }) {
                return Some(value);
            }
        }
        None
    }

    /// Attempts to select the provided event node given constraints, creating an associated job if possible.
    #[inline(always)]
    unsafe fn try_select_job(mut params: JobSelectionParams<'_>) -> Option<EventJob> {
        let deps = params
            .ctx
            .transitive_dependencies_bi
            .get_unchecked(params.node.handler.system_id as usize);
        let deps_mut = params
            .ctx
            .transitive_dependencies_mut
            .get_unchecked(params.node.handler.system_id as usize);

        if params.node.state == EventState::Queued
            && (params.id == params.start
                || (Self::bitmaps_exclusive(params.working_transitive_dependencies_bi, deps_mut)
                    && Self::bitmaps_exclusive(params.working_transitive_dependencies_mut, deps)))
        {
            if params.main_thread
                || *params
                    .ctx
                    .sync_systems
                    .get_unchecked(params.node.handler.system_id as usize)
            {
                params.node.state = EventState::Processing;
                return Some(EventJob {
                    event: params.node.event.as_ref().unwrap_unchecked().clone(),
                    handler: params.node.handler,
                    id: params.id,
                });
            } else {
                *params.main_thread_required = (*params.main_thread_required).min(params.id);
            }
        }

        *params.working_transitive_dependencies_bi |= deps;
        *params.working_transitive_dependencies_mut |= deps_mut;

        None
    }

    /// Queues newly-received events into the event pipeline.
    #[inline(always)]
    unsafe fn queue_new_jobs(&mut self, ctx: &ContextInner, main_thread: bool) -> Option<EventJob> {
        loop {
            while let Some(event) = self.event_lists.last_mut().unwrap_unchecked().pop_front() {
                if Self::clogging_event(&*event) {
                    self.clogged_event = Some(event);
                    return None;
                } else if event.is::<notify::Delayed>() {
                    if self.in_progress.is_empty() {
                        self.event_lists
                            .last_mut()
                            .unwrap_unchecked()
                            .push_back(event.downcast::<notify::Delayed>().unwrap_unchecked().0);
                    } else {
                        self.in_progress.push_back(EventNode::completed(event));
                    }
                } else {
                    let shared = Arc::<dyn Any + Send + Sync>::from(event);
                    let start = *self.in_progress.counter();
                    let end = start + self.in_progress.len() as isize;

                    for handle in ctx.event_handlers.handlers((*shared).type_id()) {
                        self.in_progress
                            .push_back(EventNode::new(shared.clone(), *handle));
                    }

                    for id in end..self.in_progress.end_index() {
                        if let Some(job) = Self::try_select_job(JobSelectionParams {
                            ctx,
                            id,
                            start,
                            node: self.in_progress.get_mut(id).unwrap_unchecked(),
                            main_thread,
                            working_transitive_dependencies_bi: &mut self
                                .working_transitive_dependencies_bi,
                            working_transitive_dependencies_mut: &mut self
                                .working_transitive_dependencies_mut,
                            main_thread_required: &mut self.blocked_main_event,
                        }) {
                            return Some(job);
                        }
                    }
                }
            }

            if self.in_progress.is_empty() {
                self.event_lists.pop();

                if self.event_lists.is_empty() {
                    break;
                }
            } else {
                break;
            }
        }

        None
    }

    /// Drops all events at the front of the in-progress queue that have completed, adding their events
    /// to the emitted events list.
    #[inline(always)]
    unsafe fn drop_front(&mut self) {
        while let Some(event) = self.in_progress.front() {
            if event.state == EventState::Complete {
                self.event_lists.last_mut().unwrap_unchecked().extend(
                    self.in_progress
                        .pop_front()
                        .unwrap_unchecked()
                        .emitted_events,
                );
            } else {
                break;
            }
        }
    }

    /// Determines whether the given event should clog the system and force an event pipeline stall.
    #[inline(always)]
    fn clogging_event(event: &dyn Any) -> bool {
        event.is::<notify::AddSystem>()
            || event.is::<notify::RemoveSystem>()
            || event.is::<notify::ResetSystem>()
            || event.is::<notify::Flush>()
    }

    /// Determines whether the two bitmaps are completely exclusive, and do not share any bits.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, the slices must match in length.
    #[inline(always)]
    unsafe fn bitmaps_exclusive(a: &BitSlice, b: &BitSlice) -> bool {
        for chunk in a.chunks(usize::BITS as usize) {
            let start = chunk.as_bitptr().offset_from(a.as_bitptr()) as usize;
            let b = &*bitslice_from_raw_parts(b.as_bitptr().add(start), chunk.len());

            if (chunk.load::<usize>() & b.load::<usize>()).count_ones() > 0 {
                return false;
            }
        }

        true
    }
}

/// Determines how a job should be selected.
struct JobSelectionParams<'a> {
    /// The inner context with which the job is associated.
    ctx: &'a ContextInner,
    /// The ID of the event node.
    id: isize,
    /// The ID of the first event node in the pipeline.
    start: isize,
    /// The event node describing the job.
    node: &'a mut EventNode,
    /// Whether this is executing on the main thread.
    main_thread: bool,
    /// The set of rolling bidirectional transitive dependencies for all previous nodes.
    working_transitive_dependencies_bi: &'a mut BitVec,
    /// The set of rolling mutable transitive dependencies for all previous nodes.
    working_transitive_dependencies_mut: &'a mut BitVec,
    /// The ID of the earliest job that requires the main thread.
    main_thread_required: &'a mut isize,
}

/// Represents the status of the event manager after finishing an event cycle.
pub enum EventManagerState {
    /// The cycle is complete, and all events have been processed.
    Complete(),
    /// The cycle is clogged and requires individual processing of the given event.
    CycleClogged(Box<dyn Any + Send + Sync>),
}

/// Denotes an event handler to execute.
pub(crate) struct EventJob {
    /// The event that provoked this handler.
    pub event: Arc<dyn Any + Send + Sync>,
    /// The handler to execute.
    pub handler: EventHandlerEntry,
    /// The ID of the job.
    pub id: isize,
}

impl EventJob {
    /// Executes this event job with the provided inner context.
    ///
    /// # Safety
    ///
    /// This job must describe a valid system and event handler presently stored in the inner context.
    #[inline(always)]
    pub unsafe fn execute(&self, ctx: &ContextInner) {
        self.handler.handler.invoke(
            transmute::<_, &mut (*mut (), *const ())>(
                ctx.systems
                    .get_unchecked(self.handler.system_id as usize)
                    .value
                    .borrow_mut()
                    .assume_init_mut(),
            )
            .0,
            &*self.event,
        );
    }
}

/// Provides a reason that a new job could not be created.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EventJobError {
    /// All events are busy.
    Busy,
    /// The event manager has completed an event cycle.
    Complete,
    /// The main thread is required to resolve blocking events in the queue.
    MainThreadRequired,
}

/// Describes the status of a received event.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum EventState {
    /// The event is queued for execution.
    #[default]
    Queued,
    /// A thread has selected this event and is running the handler.
    Processing,
    /// The event handler has run to completion.
    Complete,
}

/// Represents an event that has been queued for processing.
struct EventNode {
    /// The set of events that were raised while processing this one.
    pub emitted_events: SmallVec<[Box<dyn Any + Send + Sync>; Self::DEFAULT_EVENT_BUFFER_SIZE]>,
    /// The event that should be run, or none if this was an immediately-complete event.
    pub event: Option<Arc<dyn Any + Send + Sync>>,
    /// The event handler to run.
    pub handler: EventHandlerEntry,
    /// The state of the event.
    pub state: EventState,
}

impl EventNode {
    /// Creates a new event node in the queued state, for the given event and handler.
    #[inline(always)]
    pub fn new(event: Arc<dyn Any + Send + Sync>, handler: EventHandlerEntry) -> Self {
        Self {
            emitted_events: SmallVec::new(),
            event: Some(event),
            handler,
            state: EventState::Queued,
        }
    }

    /// Creates a new immediately-complete event that emits the provided object as a follow-up event.
    #[inline(always)]
    pub fn completed(event: Box<dyn Any + Send + Sync>) -> Self {
        let mut emitted_events = SmallVec::new();
        emitted_events.push(event);

        Self {
            emitted_events,
            event: None,
            handler: EventHandlerEntry::default(),
            state: EventState::Complete,
        }
    }
}

impl EventNode {
    /// The default amount of space to allocate for events raised as the result of another event.
    const DEFAULT_EVENT_BUFFER_SIZE: usize = 2;
}

/// Holds an event and describes the situation that raised it.
pub struct Event {
    /// The value of the event.
    pub value: Box<dyn Any + Send + Sync>,
    /// The thread on which the event was sent.
    pub sender: std::thread::ThreadId,
}

impl Event {
    /// Creates a new event that is associated with the present thread.
    #[inline(always)]
    pub fn new(value: Box<dyn Any + Send + Sync>) -> Self {
        Self {
            value,
            sender: std::thread::current().id(),
        }
    }
}

/// Manages the receipt, storage, and demuxing of raised events.
struct EventReceiver {
    /// Internally buffered events that are waiting for another thread to fetch them.
    event_list: SmallVec<[Event; Self::DEFAULT_RECEIVED_BUFFER_SIZE]>,
    /// The MPSC receiver to which raised events are sent.
    receiver: std::sync::mpsc::Receiver<Event>,
}

impl EventReceiver {
    /// The default amount of buffer space to allocate for newly-received events.
    const DEFAULT_RECEIVED_BUFFER_SIZE: usize = 4;

    /// Creates a new, empty event receiver and an associated sender for raising events.
    #[inline(always)]
    pub fn new() -> (Self, std::sync::mpsc::Sender<Event>) {
        let event_list = SmallVec::new();
        let (sender, receiver) = std::sync::mpsc::channel();

        (
            Self {
                event_list,
                receiver,
            },
            sender,
        )
    }

    /// Creates an iterator that removes and yields all events received on the current thread.
    #[inline(always)]
    pub fn get(&mut self) -> impl '_ + Iterator<Item = Box<dyn Any + Send + Sync>> {
        let id = std::thread::current().id();
        self.drain_events(id)
            .into_iter()
            .chain(ReceiverIter { id, receiver: self })
    }

    /// Creates an iterator that removes and yields all events received.
    #[inline(always)]
    pub fn get_all(&mut self) -> impl '_ + Iterator<Item = Box<dyn Any + Send + Sync>> {
        self.event_list
            .drain(..)
            .map(|x| x.value)
            .chain(self.receiver.try_iter().map(|x| x.value))
    }

    /// Drains all stored events from the buffer which correspond to the current thread, yielding them as a vector.
    #[inline(always)]
    fn drain_events(
        &mut self,
        id: std::thread::ThreadId,
    ) -> SmallVec<[Box<dyn Any + Send + Sync>; Self::DEFAULT_RECEIVED_BUFFER_SIZE]> {
        unsafe {
            let mut res = SmallVec::new();
            let mut place = 0;
            let len = self.event_list.len();

            let events = transmute::<_, &mut [MaybeUninit<Event>]>(&mut self.event_list[..]);

            for i in 0..len {
                let event = events.get_unchecked_mut(i);
                if event.assume_init_ref().sender == id {
                    res.push(event.assume_init_read().value);
                } else {
                    events.swap(place, i);
                    place += 1;
                }
            }

            self.event_list.set_len(place);
            res
        }
    }
}

/// Iterates over all new events received from an MPSC receiver, yielding the relevant ones
/// and storing the others in the event receiver's buffer.
struct ReceiverIter<'a> {
    /// The ID of the current thread.
    id: std::thread::ThreadId,
    /// The event receiver to which irrelevant events should be added.
    receiver: &'a mut EventReceiver,
}

impl<'a> Iterator for ReceiverIter<'a> {
    type Item = Box<dyn Any + Send + Sync>;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.receiver.receiver.try_recv() {
                Ok(event) => {
                    if event.sender == self.id {
                        return Some(event.value);
                    } else {
                        self.receiver.event_list.push(event);
                    }
                }
                _ => return None,
            }
        }
    }
}

/// Wraps an event manager mutex and condition variable, marking them as threadsafe.
pub(crate) struct EventManagerWrapper(
    pub wasm_sync::Mutex<*mut EventManager>,
    pub wasm_sync::Condvar,
);

unsafe impl Send for EventManagerWrapper {}
unsafe impl Sync for EventManagerWrapper {}
