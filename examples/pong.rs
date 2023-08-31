use geese::*;
use macroquad::prelude::*;


pub struct GameGraphics {
    ctx: GeeseContextHandle<Self>,
    draw_rect: Rect,
    winning_player: Option<u32>
}

impl GameGraphics {
    fn update_draw_rect(&mut self) {
        let side_length = screen_height().min(screen_width());
        self.draw_rect = Rect::new(0.5 * (screen_width() - side_length), 0.5 * (screen_height() - side_length), side_length, side_length);
    }

    fn relative_to_absolute(&self, rect: &Rect) -> Rect {
        Rect::new(self.draw_rect.w * rect.x + self.draw_rect.x, self.draw_rect.h * rect.y + self.draw_rect.y, self.draw_rect.w * rect.w, self.draw_rect.h * rect.h)
    }
    
    fn draw_rect(&self, rect: &Rect, color: Color) {
        let absolute = self.relative_to_absolute(rect);
        draw_rectangle(absolute.x, absolute.y, absolute.w, absolute.h, color);
    }

    fn draw_world(&self) {
        let world = self.ctx.get::<Store<GameWorld>>();
        
        self.draw_rect(&world.left_paddle_rect(), WHITE);
        self.draw_rect(&world.right_paddle_rect(), WHITE);

        self.draw_rect(&world.ball_rect(), WHITE);
    }

    fn end_game(&mut self, event: &on::GameOver) {
        if self.winning_player.is_none() {
            self.winning_player = Some(event.winning_player);
        }
    }

    fn draw_game(&mut self, _: &on::NewFrame) {
        self.update_draw_rect();

        if let Some(winner) = self.winning_player {
            clear_background(VIOLET);
            draw_text(&format!("Player {winner} wins!"), self.draw_rect.center().x, self.draw_rect.center().y, 30.0, WHITE);
        }
        else {
            clear_background(DARKBLUE);
            draw_rectangle_lines(self.draw_rect.x, self.draw_rect.y, self.draw_rect.w, self.draw_rect.h, 1.0, WHITE);
            self.draw_world();
        }
    }
}

impl GeeseSystem for GameGraphics {
    const DEPENDENCIES: Dependencies = dependencies()
        .with::<Store<GameWorld>>();

    const EVENT_HANDLERS: EventHandlers<Self> = event_handlers()
        .with(Self::draw_game)
        .with(Self::end_game);

    fn new(ctx: GeeseContextHandle<Self>) -> Self {
        let draw_rect = Rect::default();
        let winning_player = None;

        Self { ctx, draw_rect, winning_player }
    }
}

pub struct GameInput {
    ctx: GeeseContextHandle<Self>
}

impl GameInput {
    fn move_paddles(&mut self, event: &on::NewFrame) {
        let mut world = self.ctx.get_mut::<Store<GameWorld>>();
        let movement = world.paddle_move_speed * event.delta_time;

        if is_key_down(KeyCode::W) {
            world.left_paddle_pos -= movement;
        }
        if is_key_down(KeyCode::S) {
            world.left_paddle_pos += movement;
        }

        if is_key_down(KeyCode::Up) {
            world.right_paddle_pos -= movement;
        }
        if is_key_down(KeyCode::Down) {
            world.right_paddle_pos += movement;
        }

        let min_height = 0.5 * world.paddle_height;
        let max_height = 1.0 - min_height;

        world.left_paddle_pos = world.left_paddle_pos.clamp(min_height, max_height);
        world.right_paddle_pos = world.right_paddle_pos.clamp(min_height, max_height);
    }
}

impl GeeseSystem for GameInput {
    const DEPENDENCIES: Dependencies = dependencies()
        .with::<Mut<Store<GameWorld>>>();

    const EVENT_HANDLERS: EventHandlers<Self> = event_handlers()
        .with(Self::move_paddles);

    fn new(ctx: GeeseContextHandle<Self>) -> Self {
        Self { ctx }
    }
}

pub struct GamePhysics {
    ctx: GeeseContextHandle<Self>
}

impl GamePhysics {
    fn collide_ball_with_paddles(&mut self) {
        let mut world = self.ctx.get_mut::<Store<GameWorld>>();

        let ball_rect = world.ball_rect();

        let collision = ball_rect.overlaps(&if world.ball_velocity.x < 0.0 {
            world.left_paddle_rect()
        }
        else {
            world.right_paddle_rect()
        });

        if collision {
            world.ball_velocity.x *= -1.0;
            world.ball_velocity.y = rand::gen_range(-world.max_ball_velocity.y, world.max_ball_velocity.y);
        }
    }

    fn collide_ball_with_walls(&mut self) {
        let mut world = self.ctx.get_mut::<Store<GameWorld>>();

        let ball_rect = world.ball_rect();

        let vertical_collision = if world.ball_velocity.y < 0.0 {
            ball_rect.bottom() <= 0.0
        }
        else {
            ball_rect.top() >= 1.0
        };

        if vertical_collision {
            world.ball_velocity.y *= -1.0;
        }
        
        if world.ball_velocity.x < 0.0 {
            if ball_rect.left() <= 0.0 {
                drop(world);
                self.ctx.raise_event(on::GameOver { winning_player: 2 });
            }
        }
        else {
            if ball_rect.right() >= 1.0 {
                drop(world);
                self.ctx.raise_event(on::GameOver { winning_player: 1 });
            }
        }
    }

    fn move_ball_position(&mut self, frame: &on::NewFrame) {
        let mut world = self.ctx.get_mut::<Store<GameWorld>>();
        let move_delta = frame.delta_time * world.ball_velocity;
        world.ball_position += move_delta;
    }

    fn move_ball(&mut self, frame: &on::NewFrame) {
        self.move_ball_position(frame);
        self.collide_ball_with_paddles();
        self.collide_ball_with_walls();
    }
}

impl GeeseSystem for GamePhysics {
    const DEPENDENCIES: Dependencies = dependencies()
        .with::<Mut<Store<GameWorld>>>();

    const EVENT_HANDLERS: EventHandlers<Self> = event_handlers()
        .with(Self::move_ball);

    fn new(ctx: GeeseContextHandle<Self>) -> Self {
        Self { ctx }
    }
}

#[derive(Clone, Debug)]
pub struct GameWorld {
    pub left_paddle_pos: f32,
    pub right_paddle_pos: f32,
    pub paddle_width: f32,
    pub paddle_height: f32,
    pub paddle_move_speed: f32,
    pub ball_position: Vec2,
    pub ball_velocity: Vec2,
    pub ball_size: f32,
    pub max_ball_velocity: Vec2
}

impl GameWorld {
    pub fn left_paddle_rect(&self) -> Rect {
        Rect::new(0.0, self.left_paddle_pos - 0.5 * self.paddle_height, self.paddle_width, self.paddle_height)
    }

    pub fn right_paddle_rect(&self) -> Rect {
        Rect::new(1.0 - self.paddle_width, self.right_paddle_pos - 0.5 * self.paddle_height, self.paddle_width, self.paddle_height)
    }

    pub fn ball_rect(&self) -> Rect {
        Rect::new(self.ball_position.x - 0.5 * self.ball_size, self.ball_position.y - 0.5 * self.ball_size, self.ball_size, self.ball_size)
    }
}

impl Default for GameWorld {
    fn default() -> Self {
        Self {
            left_paddle_pos: 0.5,
            right_paddle_pos: 0.5,
            paddle_height: 0.2,
            paddle_width: 0.05,
            paddle_move_speed: 0.5,
            ball_position: vec2(0.5, 0.5),
            ball_velocity: vec2(0.5, 0.0),
            ball_size: 0.025,
            max_ball_velocity: vec2(0.7, 1.0)
        }
    }
}

pub struct PongGame;

impl GeeseSystem for PongGame {
    const DEPENDENCIES: Dependencies = dependencies()
        .with::<GameGraphics>()
        .with::<GameInput>()
        .with::<GamePhysics>();

    fn new(_: GeeseContextHandle<Self>) -> Self {
        Self
    }
}

mod on {
    pub struct NewFrame {
        pub delta_time: f32
    }

    pub struct GameOver {
        pub winning_player: u32
    }
}

#[macroquad::main("Pong")]
async fn main() {
    let mut ctx = GeeseContext::default();
    ctx.flush().with(geese::notify::add_system::<PongGame>());

    loop {
        ctx.flush().with(on::NewFrame { delta_time: get_frame_time() });
        next_frame().await;
    }
}
