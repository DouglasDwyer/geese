use bitvec::prelude::*;
use bitvec::ptr::*;
use crate::*;
use crate::arena_array::*;
use smallvec::*;
use std::collections::vec_deque::*;
use vecdeque_stableix::*;

pub(crate) struct EventManager {
    clogged_event: Option<Box<dyn Any + Send + Sync>>,
    event_lists: SmallVec<[VecDeque<Box<dyn Any + Send + Sync>>; Self::DEFAULT_EVENT_BUFFER_SIZE]>,
    receiver: EventReceiver,
    in_progress: Deque<EventNode, isize>,
    working_transitive_dependencies_bi: BitVec,
    working_transitive_dependencies_mut: BitVec
}

impl EventManager {
    const DEFAULT_EVENT_BUFFER_SIZE: usize = 4;

    pub fn new() -> (Self, std::sync::mpsc::Sender<Event>) {
        let (receiver, sender) = EventReceiver::new();

        (Self {
            clogged_event: None,
            event_lists: SmallVec::new(),
            receiver,
            in_progress: Deque::new(),
            working_transitive_dependencies_bi: BitVec::new(),
            working_transitive_dependencies_mut: BitVec::new()
        }, sender)
    }

    pub fn configure(&mut self, ctx: &ContextInner) {
        debug_assert!(self.in_progress.is_empty(), "In progress queue was not empty when configuring event manager.");

        *self.in_progress.counter_mut() = isize::MIN;
        self.working_transitive_dependencies_bi.resize(ctx.system_initializers.len(), false);
        self.working_transitive_dependencies_mut.resize(ctx.system_initializers.len(), false);
    }

    pub unsafe fn update_state(&mut self) -> EventManagerState {
        if self.clogged_event.is_some() {
            EventManagerState::CycleClogged(take(&mut self.clogged_event).unwrap_unchecked())
        }
        else {
            self.clear_completed_cycles();

            if self.event_lists.is_empty() {
                EventManagerState::Complete()
            }
            else {
                EventManagerState::CyclesPending()
            }
        }
    }

    pub unsafe fn gather_external_events(&mut self) {
        self.event_lists.last_mut().unwrap_unchecked().extend(self.receiver.get_all().collect::<VecDeque<_>>());
    }

    pub fn push_external_event_cycle(&mut self) {
        unsafe {
            debug_assert!(self.in_progress.is_empty(), "In progress queue was not empty when starting a new event cycle.");
    
            self.clear_completed_cycles();
            self.event_lists.push(VecDeque::new());
        }
    }

    pub fn push_event_cycle(&mut self, events: impl Iterator<Item = Box<dyn Any + Send + Sync>>) {
        unsafe {
            debug_assert!(self.in_progress.is_empty(), "In progress queue was not empty when starting a new event cycle.");
    
            self.clear_completed_cycles();
            self.event_lists.push(VecDeque::new());
            self.event_lists.last_mut().unwrap_unchecked().extend(events);
        }
    }

    pub unsafe fn next_job(&mut self, ctx: &ContextInner) -> Result<EventJob, EventJobError> {
        let main_thread = std::thread::current().id() == ctx.owning_thread;
        
        self.working_transitive_dependencies_bi.fill(false);
        self.working_transitive_dependencies_mut.fill(false);
        
        if let Some(result) = self.get_queued_job(ctx, main_thread) {
            return Ok(result);
        }

        if self.clogged_event.is_none() {
            if let Some(result) = self.queue_new_jobs(ctx, main_thread) {
                return Ok(result);
            }
        }

        if self.in_progress.is_empty() {
            Err(EventJobError::Complete)
        }
        else {
            Err(EventJobError::Busy)
        }
    }

    pub unsafe fn complete_job(&mut self, job: &EventJob) {
        let front_ev = self.in_progress.front().unwrap_unchecked().event.clone();
        let value = self.in_progress.get_mut(job.id).unwrap_unchecked();
        value.state = EventState::Complete;

        let emitted = self.receiver.get();
        if Arc::ptr_eq(&value.event, &front_ev) {
            self.event_lists.last_mut().unwrap_unchecked().extend(emitted);

            if job.id == *self.in_progress.counter() {
                self.drop_front();
            }
        }
        else {
            value.emitted_events.extend(emitted);
        }
    }

    unsafe fn clear_completed_cycles(&mut self) {
        while !self.event_lists.is_empty() && self.event_lists.last().unwrap_unchecked().is_empty() {
            self.event_lists.pop();
        }
    }

    unsafe fn get_queued_job(&mut self, ctx: &ContextInner, main_thread: bool) -> Option<EventJob> {
        let start = *self.in_progress.counter();
        for (id, node) in self.in_progress.iter_mut().filter(|(_, event)| event.state != EventState::Complete) {
            if let Some(value) = Self::try_select_job(ctx, id, start, node, main_thread, &mut self.working_transitive_dependencies_bi, &mut self.working_transitive_dependencies_mut) {
                return Some(value);
            }
        }
        None
    }

    unsafe fn try_select_job(ctx: &ContextInner, id: isize, start: isize, node: &mut EventNode, main_thread: bool, working_transitive_dependencies_bi: &mut BitVec, working_transitive_dependencies_mut: &mut BitVec) -> Option<EventJob> {
        let deps = ctx.transitive_dependencies_bi.get_unchecked(node.handler.system_id as usize);
        let deps_mut = ctx.transitive_dependencies_mut.get_unchecked(node.handler.system_id as usize);
        
        if node.state == EventState::Queued
            && (id == start || (Self::bitmaps_exclusive(&working_transitive_dependencies_bi, deps_mut) && Self::bitmaps_exclusive(&working_transitive_dependencies_mut, deps)))
            && (main_thread || *ctx.sync_systems.get_unchecked(node.handler.system_id as usize)) {
            node.state = EventState::Processing;
            return Some(EventJob { event: node.event.clone(), handler: node.handler, id });
        }

        *working_transitive_dependencies_bi |= deps;
        *working_transitive_dependencies_mut |= deps_mut;

        None
    }

    unsafe fn queue_new_jobs(&mut self, ctx: &ContextInner, main_thread: bool) -> Option<EventJob> {
        while let Some(event) = self.event_lists.last_mut().unwrap_unchecked().pop_front() {
            if Self::clogging_event(&*event) {
                self.clogged_event = Some(event);
                return None;
            }
            else {
                let shared = Arc::<dyn Any + Send + Sync>::from(event);
                let start = *self.in_progress.counter();
                let end = start + self.in_progress.len() as isize;

                for handle in ctx.event_handlers.handlers((*shared).type_id()) {
                    self.in_progress.push_back(EventNode::new(shared.clone(), *handle));
                }
                
                for id in end..self.in_progress.end_index() {
                    if let Some(job) = Self::try_select_job(ctx, id, start, self.in_progress.get_mut(id).unwrap_unchecked(), main_thread, &mut self.working_transitive_dependencies_bi, &mut self.working_transitive_dependencies_mut) {
                        return Some(job);
                    }
                }
            }
        }

        None
    }

    unsafe fn drop_front(&mut self) {
        while let Some(event) = self.in_progress.front() {
            if event.state == EventState::Complete {
                drop(event);
                self.event_lists.last_mut().unwrap_unchecked().extend(self.in_progress.pop_front().unwrap_unchecked().emitted_events);
            }
        }
    }

    fn clogging_event(event: &dyn Any) -> bool {
        if event.is::<notify::AddSystem>()
            || event.is::<notify::RemoveSystem>()
            || event.is::<notify::ResetSystem>()
            || event.is::<notify::Flush>() {
            true
        }
        else {
            false
        }
    }

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

pub enum EventManagerState {
    Complete(),
    CycleClogged(Box<dyn Any + Send + Sync>),
    CyclesPending(),
}

pub(crate) struct EventJob {
    pub event: Arc<dyn Any + Send + Sync>,
    pub handler: EventHandlerEntry,
    pub id: isize
}

impl EventJob {
    pub unsafe fn execute(&self, ctx: &ContextInner) {
        self.handler.handler.invoke(transmute::<_, &mut (*mut (), *const ())>(ctx.systems.get_unchecked(self.handler.system_id as usize).value.borrow_mut().assume_init_mut()).0, &*self.event);        
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EventJobError {
    Busy,
    Complete
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum EventState {
    #[default]
    Queued,
    Processing,
    Complete
}

struct EventNode {
    pub emitted_events: SmallVec<[Box<dyn Any + Send + Sync>; Self::DEFAULT_EVENT_BUFFER_SIZE]>,
    pub event: Arc<dyn Any + Send + Sync>,
    pub handler: EventHandlerEntry,
    pub state: EventState
}

impl EventNode {
    pub fn new(event: Arc<dyn Any + Send + Sync>, handler: EventHandlerEntry) -> Self {
        Self {
            emitted_events: SmallVec::new(),
            event,
            handler,
            state: EventState::Queued
        }
    }
}

impl EventNode {
    const DEFAULT_EVENT_BUFFER_SIZE: usize = 2;
}

pub struct Event {
    value: Box<dyn Any + Send + Sync>,
    sender: std::thread::ThreadId
}

impl Event {
    pub fn new(value: Box<dyn Any + Send + Sync>) -> Self {
        Self {
            value,
            sender: std::thread::current().id()
        }
    }
}

struct EventReceiver {
    event_list: SmallVec<[Event; Self::DEFAULT_RECEIVED_BUFFER_SIZE]>,
    receiver: std::sync::mpsc::Receiver<Event>
}

impl EventReceiver {
    const DEFAULT_RECEIVED_BUFFER_SIZE: usize = 4;

    pub fn new() -> (Self, std::sync::mpsc::Sender<Event>) {
        let event_list = SmallVec::new();
        let (sender, receiver) = std::sync::mpsc::channel();

        (Self { event_list, receiver }, sender)
    }

    pub fn get(&mut self) -> impl '_ + Iterator<Item = Box<dyn Any + Send + Sync>> {
        let id = std::thread::current().id();
        self.drain_events(id).into_iter().chain(ReceiverIter { id, receiver: self })
    }

    pub fn get_all(&mut self) -> impl '_ + Iterator<Item = Box<dyn Any + Send + Sync>> {
        self.event_list.drain(..).map(|x| x.value).chain(self.receiver.try_iter().map(|x| x.value))
    }

    fn drain_events(&mut self, id: std::thread::ThreadId) -> SmallVec<[Box<dyn Any + Send + Sync>; Self::DEFAULT_RECEIVED_BUFFER_SIZE]> {
        unsafe {
            let mut res = SmallVec::new();
            let mut place = 0;
            let len = self.event_list.len();

            let events = transmute::<_, &mut [MaybeUninit<Event>]>(&mut self.event_list[..]);

            for i in 0..len {
                let event = events.get_unchecked_mut(i);
                if event.assume_init_ref().sender == id {
                    res.push(event.assume_init_read().value);
                }
                else {
                    events.swap(place, i);
                    place += 1;
                }
            }

            self.event_list.set_len(place);
            res
        }
    }
}

struct ReceiverIter<'a> {
    id: std::thread::ThreadId,
    receiver: &'a mut EventReceiver
}

impl<'a> Iterator for ReceiverIter<'a> {
    type Item = Box<dyn Any + Send + Sync>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.receiver.receiver.try_recv() {
                Ok(event) => {
                    if event.sender == self.id {
                        return Some(event.value);
                    }
                    else {
                        self.receiver.event_list.push(event);
                    }
                },
                _ => return None
            }
        }
    }
}