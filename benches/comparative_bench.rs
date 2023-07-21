use criterion::*;
use geese::*;
use std::marker::*;
use std::sync::*;
use std::sync::atomic::*;
use std::ops::*;

fn fibonacci(n: u64) -> u64 {
    match n {
        0 => 1,
        1 => 1,
        n => fibonacci(n - 1) + fibonacci(n - 2),
    }
}

struct SpinSystem<C: Counter> {
    ctx: GeeseContextHandle<Self>,
    on_spin: Option<Arc<dyn Fn() + Send + Sync>>,
    data: PhantomData<fn(C)>
}

impl<C: Counter> SpinSystem<C> {
    fn spin(&mut self, value: &C) {
        if value.is_nonzero() {
            self.ctx.raise_event(value.decrement());
            if let Some(spin) = &self.on_spin {
                spin();
            }
        }
    }
}

impl<C: Counter> GeeseSystem for SpinSystem<C> {
    const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new()
        .with(Self::spin);

    fn new(ctx: GeeseContextHandle<Self>) -> Self {
        Self { ctx, data: PhantomData, on_spin: None }
    }
}

struct OldSpinSystem<C: Counter> {
    ctx: old_geese::GeeseContextHandle,
    on_spin: Option<Arc<dyn Fn() + Send + Sync>>,
    data: PhantomData<fn(C)>
}

impl<C: Counter> OldSpinSystem<C> {
    fn spin(&mut self, value: &C) {
        if value.is_nonzero() {
            self.ctx.raise_event(value.decrement());
            if let Some(spin) = &self.on_spin {
                spin();
            }
        }
    }
}

impl<C: Counter> old_geese::GeeseSystem for OldSpinSystem<C> {
    fn new(ctx: old_geese::GeeseContextHandle) -> Self {
        Self { ctx, data: PhantomData, on_spin: None }
    }

    fn register(with: &mut old_geese::GeeseSystemData<Self>) {
        with.event(Self::spin);
    }
}

fn bench_spin(c: &mut Criterion) {
    let mut old_ctx = old_geese::GeeseContext::default();
    old_ctx.raise_event(old_geese::notify::add_system::<OldSpinSystem<usize>>());
    old_ctx.flush_events();

    c.bench_function("old_spin", |b| b.iter(|| {
        old_ctx.raise_event(1000usize);
        old_ctx.flush_events();
    }));
    
    let mut ctx = GeeseContext::default();
    ctx.raise_event(geese::notify::add_system::<SpinSystem<usize>>());
    ctx.flush_events();

    c.bench_function("new_spin", |b| b.iter(|| {
        ctx.raise_event(1000usize);
        ctx.flush_events();
    }));
}

fn bench_double(c: &mut Criterion) {
    let mut old_ctx = old_geese::GeeseContext::default();
    old_ctx.raise_event(old_geese::notify::add_system::<OldSpinSystem<usize>>());
    old_ctx.raise_event(old_geese::notify::add_system::<OldSpinSystem<isize>>());
    old_ctx.flush_events();

    c.bench_function("double_old", |b| b.iter(|| {
        old_ctx.raise_event(1000usize);
        old_ctx.raise_event(1000isize);
        old_ctx.flush_events();
    }));
    
    let mut ctx = GeeseContext::default();
    ctx.raise_event(geese::notify::add_system::<SpinSystem<usize>>());
    ctx.raise_event(geese::notify::add_system::<SpinSystem<isize>>());
    ctx.flush_events();
    
    c.bench_function("double_new", |b| b.iter(|| {
        ctx.raise_event(1000usize);
        ctx.raise_event(1000isize);
        ctx.flush_events();
    }));

    ctx.set_threadpool(HardwareThreadPool::new(1));
    
    c.bench_function("double_new_multi", |b| b.iter(|| {
        ctx.raise_event(1000usize);
        ctx.raise_event(1000isize);
        ctx.flush_events();
    }));
}

fn bench_double_work(c: &mut Criterion) {
    let work = Arc::new(|| { black_box(fibonacci(black_box(18))); });

    let mut old_ctx = old_geese::GeeseContext::default();
    old_ctx.raise_event(old_geese::notify::add_system::<OldSpinSystem<usize>>());
    old_ctx.raise_event(old_geese::notify::add_system::<OldSpinSystem<isize>>());
    old_ctx.flush_events();

    old_ctx.system_mut::<OldSpinSystem<usize>>().on_spin = Some(work.clone());
    old_ctx.system_mut::<OldSpinSystem<isize>>().on_spin = Some(work.clone());

    c.bench_function("work_old_double", |b| b.iter(|| {
        old_ctx.raise_event(1000usize);
        old_ctx.raise_event(1000isize);
        old_ctx.flush_events();
    }));
    
    let mut ctx = GeeseContext::default();
    ctx.raise_event(geese::notify::add_system::<SpinSystem<usize>>());
    ctx.raise_event(geese::notify::add_system::<SpinSystem<isize>>());
    ctx.flush_events();
    
    ctx.system_mut::<SpinSystem<usize>>().on_spin = Some(work.clone());
    ctx.system_mut::<SpinSystem<isize>>().on_spin = Some(work.clone());
    
    c.bench_function("work_new_double", |b| b.iter(|| {
        ctx.raise_event(1000usize);
        ctx.raise_event(1000isize);
        ctx.flush_events();
    }));

    ctx.set_threadpool(HardwareThreadPool::new(1));
    
    c.bench_function("work_new_multi", |b| b.iter(|| {
        ctx.raise_event(1000usize);
        ctx.raise_event(1000isize);
        ctx.flush_events();
    }));
}

trait Counter: 'static + Copy + Send + Sync + Sized {
    fn decrement(self) -> Self;
    fn is_nonzero(self) -> bool;
}

impl Counter for isize {
    fn decrement(self) -> Self {
        self - 1
    }

    fn is_nonzero(self) -> bool {
        self != 0
    }
}

impl Counter for usize {
    fn decrement(self) -> Self {
        self - 1
    }

    fn is_nonzero(self) -> bool {
        self != 0
    }
}

struct TestDependency {
    ctx: GeeseContextHandle<Self>
}

impl TestDependency {
    fn get_it(&mut self, event: &()) {
        for i in 0..1000 {
            self.ctx.get::<TestDependent>().test();
        }
    }
}

impl GeeseSystem for TestDependency {
    const DEPENDENCIES: Dependencies = Dependencies::new()
        .with::<TestDependent>();

    const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new()
        .with(Self::get_it);

    fn new(ctx: GeeseContextHandle<Self>) -> Self {
        Self { ctx }
    }
}

struct TestDependencyOld {
    ctx: old_geese::GeeseContextHandle
}

impl TestDependencyOld {
    fn get_it(&mut self, event: &()) {
        for i in 0..1000 {
            self.ctx.system::<TestDependent>().test();
        }
    }
}

impl old_geese::GeeseSystem for TestDependencyOld {
    fn new(ctx: old_geese::GeeseContextHandle) -> Self {
        Self { ctx }
    }

    fn register(with: &mut old_geese::GeeseSystemData<Self>) {
        with.dependency::<TestDependent>();

        with.event(Self::get_it);
    }
}

struct TestDependent;

impl TestDependent {
    pub fn test(&self) {
        black_box(self);
    }
}

impl GeeseSystem for TestDependent {
    fn new(_: GeeseContextHandle<Self>) -> Self {
        Self
    }
}

impl old_geese::GeeseSystem for TestDependent {
    fn new(_: old_geese::GeeseContextHandle) -> Self {
        Self
    }
}

fn bench_dep_fetch(c: &mut Criterion) {
    let mut old_ctx = old_geese::GeeseContext::default();
    old_ctx.raise_event(old_geese::notify::add_system::<TestDependencyOld>());
    old_ctx.flush_events();

    c.bench_function("dep_fetch_old", |b| b.iter(|| {
        old_ctx.raise_event(());
        old_ctx.flush_events();
    }));
    
    let mut ctx = GeeseContext::default();
    ctx.raise_event(geese::notify::add_system::<TestDependency>());
    ctx.flush_events();

    c.bench_function("dep_fetch_new", |b| b.iter(|| {
        old_ctx.raise_event(());
        ctx.flush_events();
    }));
}

criterion_group!(benches, bench_spin, bench_double, bench_double_work, bench_dep_fetch);
criterion_main!(benches);