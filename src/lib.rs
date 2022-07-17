use std::any::*;
use std::cell::*;
use std::collections::*;
use std::rc::*;

pub struct SystemManager<T> {
    state: Rc<RefCell<T>>,
    sink: Rc<RefCell<EventSink>>,
    systems: Vec<Box<dyn SystemHolder>>
}

impl<T: 'static> SystemManager<T> {
    pub fn new(state: T) -> Self {
        let state = Rc::new(RefCell::new(state));
        let sink = Rc::new(RefCell::new(EventSink::new()));
        let systems = Vec::new();

        Self { state, sink, systems }
    }

    pub fn register_system(&mut self, system: impl 'static + System) {
        self.systems.push(Box::new(TypedSystemHolder::new(system)));
    }

    pub fn event_sink(&mut self) -> RefMut<EventSink> {
        self.sink.borrow_mut()
    }

    pub fn state(&mut self) -> RefMut<T> {
        self.state.borrow_mut()
    }

    pub fn flush_events(&mut self) {
        let mut sink = self.drain_system_events();
        while sink.count() > 0 {
            EventDrainer::new(|sink| {
                for system in &mut self.systems {
                    system.dispatch_events(&sink, self.sink.clone(), Box::new(self.state.clone()));
                }
            }).dispatch(sink);

            sink = self.drain_system_events();
        }
    }

    fn drain_system_events(&mut self) -> EventSink {
        self.sink.replace(EventSink::new())
    }
}

pub trait System {
    fn register_events(&self, dispatcher: &mut EventDispatcher);
}

trait SystemHolder {
    fn dispatch_events(&mut self, source: &EventSink, sink: Rc<RefCell<EventSink>>, state: Box<dyn Any>);
}

struct TypedSystemHolder<T: 'static + System> {
    dispatcher: EventDispatcher,
    system: Rc<RefCell<T>>
}

impl<T: System> TypedSystemHolder<T> {
    pub fn new(system: T) -> Self {
        let mut dispatcher = EventDispatcher::new();
        system.register_events(&mut dispatcher);
        let system = Rc::new(RefCell::new(system));

        Self { dispatcher, system }
    }
}

impl<T: System> SystemHolder for TypedSystemHolder<T> {
    fn dispatch_events(&mut self, source: &EventSink, sink: Rc<RefCell<EventSink>>, state: Box<dyn Any>) {
        let args: Vec<Box<dyn Any>> = vec!(Box::new(self.system.clone()), state, Box::new(sink));
        self.dispatcher.dispatch(source, &args);
    }
}

pub struct EventHandler {
    responder: Box<dyn EventResponder>
}

impl EventHandler {
    pub fn new<S: 'static, Q: 'static, E: 'static, P: 'static>(func: fn(&mut S, &mut Q, &E, &P)) -> Self {
        let responder = Box::new(EventHandlerResponder { func });

        Self { responder }
    }
}

impl EventResponder for EventHandler {
    fn event_type(&self) -> TypeId {
        self.responder.event_type()
    }

    fn invoke(&self, state: &Vec<Box<dyn Any>>, event: &Box<dyn Any>) {
        self.responder.invoke(state, event);
    }
}

struct EventHandlerResponder<S: 'static, Q: 'static, E: 'static, P: 'static> {
    func: fn(&mut S, &mut Q, &E, &P)
}

impl<S, Q, E, P> EventHandlerResponder<S, Q, E, P> {
    fn try_invoke(&self, state: &Vec<Box<dyn Any>>, event: &Box<dyn Any>) -> Result<(), ()> {
        let sys: &Rc<RefCell<S>> = state.get(0).ok_or(())?.downcast_ref().ok_or(())?;
        let st: &Rc<RefCell<Q>> = state.get(1).ok_or(())?.downcast_ref().ok_or(())?;
        let event = event.downcast_ref().ok_or(())?;
        let sink: &Rc<RefCell<P>> = state.get(2).ok_or(())?.downcast_ref().ok_or(())?;

        (self.func)(&mut sys.borrow_mut(), &mut st.borrow_mut(), event, &sink.borrow());

        Ok(())
    } 
}

impl<S, Q, E, P> EventResponder for EventHandlerResponder<S, Q, E, P> {
    fn event_type(&self) -> TypeId {
        TypeId::of::<E>()
    }

    fn invoke(&self, state: &Vec<Box<dyn Any>>, event: &Box<dyn Any>) {
        let _ = self.try_invoke(state, event);
    }
}


pub struct EventSink {
    queue: RefCell<Vec<Box<dyn Any>>>
}

impl EventSink {
    /// Creates a new event sink.
    pub fn new() -> Self {
        let queue = RefCell::new(Vec::new());

        Self { queue }
    }

    /// Raises a new event.
    pub fn raise_event<T: 'static>(&self, event: T) {
        self.queue.borrow_mut().push(Box::new(event));
    }

    /// Raises a new event with an unknown type.
    pub fn raise_event_boxed(&self, event: Box<dyn Any>) {
        self.queue.borrow_mut().push(event);
    }

    /// Drains the contents of the other event sink into this one.
    pub fn drain(&self, other: &mut EventSink) {
        self.queue.borrow_mut().append(&mut other.queue.borrow_mut());
    }

    /// Returns the number of events currently in the sink.
    pub fn count(&self) -> usize {
        self.queue.borrow_mut().len()
    }

    /// Clears the sink.
    pub fn clear(&mut self) {
        self.queue.get_mut().clear();
    }
}

struct EventDrainer<'a> {
    responder: Box<dyn 'a + FnMut(&EventSink)>
}

impl<'a> EventDrainer<'a> {
    /// Creates a new event drainer, that responds to all events in a sink.
    pub fn new<T: 'a + FnMut(&EventSink)>(responder: T) -> Self {
        let responder = Box::new(responder);
        Self { responder }
    }

    /// Dispatches all the events in the given sink.
    pub fn dispatch(&mut self, sink: EventSink) {
        for event in sink.queue.take() {
            let sink = EventSink::new();
            sink.raise_event_boxed(event);
            (self.responder)(&sink);
        }
    }
}

pub struct EventDispatcher {
    handlers: HashMap<TypeId, Vec<Box<dyn EventResponder>>>
}

impl EventDispatcher {
    /// Creates a new event dispatcher.
    pub fn new() -> Self {
        let handlers = HashMap::new();

        Self { handlers }
    }

    /// Registers a handler for the event dispatcher.
    pub fn register_handler(&mut self, handler: impl EventResponder) {
        self.handlers.entry(handler.event_type()).or_default().push(Box::new(handler));
    }

    /// Dispatches all the events in the given sink.
    pub fn dispatch(&self, sink: &EventSink, state: &Vec<Box<dyn Any>>) {
        for event in &*sink.queue.borrow_mut() {
            if let Some(handlers) = self.handlers.get(&(**event).type_id()) {
                for handler in handlers {
                    handler.invoke(state, event);
                }
            }
        }
    }

    /// Dispatches all of the events in the given sink, and then clears it.
    pub fn dispatch_drain(&self, sink: &mut EventSink, state: &Vec<Box<dyn Any>>) {
        self.dispatch(sink, state);
        sink.clear();
    }
}

pub trait EventResponder: 'static {
    fn event_type(&self) -> TypeId;
    fn invoke(&self, state: &Vec<Box<dyn Any>>, event: &Box<dyn Any>);
}