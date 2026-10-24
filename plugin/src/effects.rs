use crate::{game::{CREATIVE_DURATION_TICKS, Position, STAR_DURATION_TICKS}, runtime};
use pumpkin_plugin_api::{
	Entity, EntityDisplayExt, EntityType, Player, Server, World,
	boss_bar::{BossBar, BossBarColor, BossBarDivision},
	common::BlockPos,
	text::TextComponent,
	persistent_data::string_tree,
	world::{Block, BlockFlags, BlockState},
};
use std::{collections::{HashMap, HashSet}, sync::{LazyLock, Mutex, MutexGuard}};

static EVENT_BAR: Mutex<Option<BossBar>> = Mutex::new(None);
type PropBars = HashMap<(String, &'static str), BossBar>;
static PROP_BARS: LazyLock<Mutex<PropBars>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static BORDER: Mutex<Option<BorderTransition>> = Mutex::new(None);
struct BorderTransition { from: f64, to: f64, elapsed: u32 }
struct FallingIron { id: u32, pos: Position, speed: f64, age: u32 }
static FALLING_IRON: Mutex<Vec<FallingIron>> = Mutex::new(Vec::new());
const IRON_NAMESPACE: &str = "pillars-of-fortune";
const IRON_MARKER: &str = "falling_iron";

pub fn clear_prop_bars(id: &str) {
	let bars: Vec<BossBar> = {
		let mut bars: MutexGuard<'_, PropBars> = PROP_BARS.lock().unwrap_or_else(|error| error.into_inner());
		["gravity", "star", "creative"].into_iter().filter_map(|prop: &'static str| bars.remove(&(id.to_string(), prop))).collect()
	};
	for bar in bars { bar.set_health(0.0); bar.remove_all(); }
}

pub fn prop_bar(server: &Server, player: &Player, prop: &'static str, remaining: u64) {
	let (name, total, color): (&str, u64, BossBarColor) = match prop {
		"gravity" => ("反重力", 200, BossBarColor::Blue),
		"star" => ("无敌星", STAR_DURATION_TICKS, BossBarColor::Yellow),
		"creative" => ("当个创世神", CREATIVE_DURATION_TICKS, BossBarColor::Green),
		_ => return,
	};
	let key: (String, &'static str) = (player.get_id().to_string(), prop);
	let previous: Option<BossBar> = PROP_BARS.lock().unwrap_or_else(|error| error.into_inner()).remove(&key);
	if remaining == 0 {
		if let Some(bar) = previous { bar.set_health(0.0); bar.remove_all(); }
		return;
	}
	let text: String = format!("{name} · 剩余 {} 秒", remaining.div_ceil(20));
	let title: TextComponent = TextComponent::text(&text);
	let bar: BossBar = if let Some(bar) = previous { bar.set_title(title); bar } else {
		let bar: BossBar = BossBar::new(title, color, BossBarDivision::NoDivision);
		if let Some(viewer) = server.get_player_by_uuid(player.get_id()) { bar.add_player(viewer); }
		bar
	};
	bar.set_health((remaining as f32 / total as f32).clamp(0.0, 1.0));
	PROP_BARS.lock().unwrap_or_else(|error| error.into_inner()).insert(key, bar);
}

fn replaceable(state: &BlockState) -> bool {
	matches!(state.block_name.trim_start_matches("minecraft:"), "air" | "cave_air" | "void_air" | "water" | "lava")
}

pub fn spawn_iron(world: &World, pos: Position) {
	let Some(block) = Block::from_name("minecraft:iron_block") else { return; };
	let pos: Position = (pos.0.floor(), (pos.1 + 15.0).min(127.0), pos.2.floor());
	let entity: Entity = world.spawn_entity(EntityType::BlockDisplay, pos);
	entity.set_has_gravity(false);
	entity.set_custom_data(IRON_NAMESPACE, IRON_MARKER, &string_tree("v1"));
	if let Some(display) = entity.as_block_display() { display.set_block_state_id(block.default_state_id); }
	FALLING_IRON.lock().unwrap_or_else(|error| error.into_inner()).push(FallingIron { id: entity.get_id(), pos, speed: 0.0, age: 0 });
}

pub fn tick_iron(server: &Server) {
	let mut falling: Vec<FallingIron> = std::mem::take(&mut *FALLING_IRON.lock().unwrap_or_else(|error| error.into_inner()));
	if falling.is_empty() { return; }
	let Some(world) = runtime::world(server) else { return; };
	let entities: HashMap<u32, Entity> = world.get_entities().into_iter().map(|entity: Entity| (entity.get_id(), entity)).collect();
	falling.retain_mut(|iron: &mut FallingIron| {
		let Some(entity) = entities.get(&iron.id) else { return false; };
		iron.age += 1;
		iron.speed = (iron.speed + 0.04) * 0.98;
		let next_y: f64 = iron.pos.1 - iron.speed;
		let x: i32 = iron.pos.0 as i32;
		let z: i32 = iron.pos.2 as i32;
		if world.get_chunk(x.div_euclid(16), z.div_euclid(16)).is_none() || next_y < -64.0 || iron.age > 300 {
			entity.remove();
			return false;
		}
		// Sweep every crossed block to avoid tunnelling through floors at high speed.
		for y in (next_y.floor() as i32..=iron.pos.1.floor() as i32).rev() {
			let below: BlockPos = BlockPos { x, y: y - 1, z };
			if y > -64 && !replaceable(&world.get_block_state(below)) && next_y <= f64::from(y) {
				let landing: BlockPos = BlockPos { x, y, z };
				if replaceable(&world.get_block_state(landing)) { world.set_block_by_name(landing, "minecraft:iron_block", BlockFlags::NOTIFY_LISTENERS); }
				entity.remove();
				return false;
			}
		}
		iron.pos.1 = next_y;
		if let Some(target_world) = runtime::world(server) { entity.teleport(iron.pos, target_world); }
		true
	});
	FALLING_IRON.lock().unwrap_or_else(|error| error.into_inner()).extend(falling);
}

pub fn clear_iron(world: &World) {
	FALLING_IRON.lock().unwrap_or_else(|error| error.into_inner()).clear();
	for entity in world.get_entities() {
		if entity.has_custom_data(IRON_NAMESPACE, IRON_MARKER) { entity.remove(); }
	}
}

pub fn event_bar(server: &Server, players: &HashMap<String, Player>, name: &str, remaining: u64, total: u64) {
	let previous: Option<BossBar> = EVENT_BAR.lock().unwrap_or_else(|error| error.into_inner()).take();
	let title_text: String = format!("{name} · 剩余 {} 秒", remaining.div_ceil(20));
	let title: TextComponent = TextComponent::text(&title_text);
	let bar: BossBar = if let Some(bar) = previous { bar.set_title(title); bar }
		else { BossBar::new(title, BossBarColor::Purple, BossBarDivision::NoDivision) };
	bar.set_health((remaining as f32 / total.max(1) as f32).clamp(0.0, 1.0));
	let viewers: Vec<Player> = bar.get_players();
	let present: HashSet<String> = viewers.iter().map(|player: &Player| player.get_id().to_string()).collect();
	for player in viewers {
		if !players.contains_key(&player.get_id().to_string()) { bar.remove_player(player); }
	}
	for (id, player) in players {
		if !present.contains(id) {
			if let Some(viewer) = server.get_player_by_uuid(player.get_id()) { bar.add_player(viewer); }
		}
	}
	*EVENT_BAR.lock().unwrap_or_else(|error| error.into_inner()) = Some(bar);
}

pub fn border(world: &World, size: f64) {
	if size < 49.0 {
		let from: f64 = world.get_border().get_diameter();
		*BORDER.lock().unwrap_or_else(|error| error.into_inner()) = Some(BorderTransition { from, to: size, elapsed: 0 });
	} else {
		*BORDER.lock().unwrap_or_else(|error| error.into_inner()) = None;
		world.get_border().set_diameter(size, None);
	}
}

// Update the host's actual size each tick. Its native lerp only animates the
// client packet and immediately changes the size used by server-side checks.
pub fn tick_border(server: &Server) {
	let size: Option<f64> = {
		let mut border: MutexGuard<'_, Option<BorderTransition>> = BORDER.lock().unwrap_or_else(|error| error.into_inner());
		if let Some(transition) = border.as_mut() {
			transition.elapsed += 1;
			let size: f64 = transition.from + (transition.to - transition.from) * f64::from(transition.elapsed) / 100.0;
			if transition.elapsed >= 100 { *border = None; }
			Some(size)
		} else { None }
	};
	if let Some(size) = size {
		if let Some(world) = runtime::world(server) { world.get_border().set_diameter(size, None); }
	}
}

pub fn end_event(world: &World) {
	clear_iron(world);
	let bar: Option<BossBar> = EVENT_BAR.lock().unwrap_or_else(|error| error.into_inner()).take();
	if let Some(bar) = bar { bar.set_health(0.0); bar.remove_all(); }
	*BORDER.lock().unwrap_or_else(|error| error.into_inner()) = None;
}
