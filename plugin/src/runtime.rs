use crate::{config::{Config, EventKind}, effects, game::{Action, CREATIVE_DURATION_TICKS, LOBBY, Position, Power, Snapshot, STAR_DURATION_TICKS, floor_position}};
use pumpkin_plugin_api::{
	Attribute, Enchantment, Entity, EntityType, ItemStack, Player, Server, TeamSettingsBuilder, World,
	common::{BlockPos, GameMode, NbtTag, NbtTree},
	world::BlockState,
	text::TextComponent,
	server::CommandSender,
	wit::pumpkin::plugin::{status_effect::{StatusEffectInstance, StatusEffectType}, uuid},
};
use std::collections::HashMap;

pub const WORLD: &str = "minecraft:overworld";

pub fn world(server: &Server) -> Option<World> { server.get_world_by_name(WORLD) }

pub fn validate_world(server: &Server) -> Result<(), String> {
	let packs: pumpkin_plugin_api::DatapackManager = server.get_datapack_manager();
	if !packs.is_enabled("file/jkpof_assets") || packs.is_enabled("file/jkpof") {
		return Err("需要启用 jkpof_assets 结构资源包，并停用原版 jkpof 游戏数据包。".to_string());
	}
	let world: World = world(server).ok_or("主世界尚未加载。")?;
	let spawn: BlockPos = world.get_spawn_location().pos;
	if spawn.x != 0 || spawn.y != 64 || spawn.z != -100 {
		return Err("请先加载 pumpkin/world：此插件要求大厅出生点为 0 64 -100。".to_string());
	}
	Ok(())
}

// get_block does not load chunks: an unloaded chunk reads as air in Pumpkin.
// Client view tickets load these chunks after a player joins the lobby.
pub fn poll_world(server: &Server) -> Result<bool, String> {
	let arena: World = world(server).ok_or("主世界已卸载。")?;
	for x in -2..=1 {
		for z in -8..=-6 {
			if arena.get_chunk(x, z).is_none() { return Ok(false); }
		}
	}
	let floor: String = arena.get_block(BlockPos { x: 0, y: 63, z: -100 }).name;
	if !floor.ends_with("light_gray_concrete") {
		return Err(format!("大厅区块已加载，但 0 63 -100 的方块是 {floor}，预期为 light_gray_concrete；请检查存档及 dimensions 目录是否覆盖了 region 目录。"));
	}
	Ok(true)
}

pub fn arena_ready(server: &Server) -> bool {
	let Some(arena) = world(server) else { return false; };
	(-2..=1).all(|x: i32| (-2..=1).all(|z: i32| arena.get_chunk(x, z).is_some()))
}

pub fn prepare_lobby(server: &Server) {
	for block in ["lime_concrete", "green_concrete", "lime_wool", "green_wool"] {
		command(server, &format!("fill -17 63 -117 17 67 -83 light_gray_concrete replace {block} strict"));
	}
}

pub fn verify_ground(server: &Server, config: &Config) -> Result<(), String> {
	let arena: World = world(server).ok_or("主世界已卸载。")?;
	let radius: i32 = config.ground_radius;
	for layer in 0..config.ground_layers {
		for (x, z) in [(0, 0), (-radius, 0), (radius, 0), (0, -radius), (0, radius)] {
			let y: i32 = -layer;
			let actual: String = arena.get_block(BlockPos { x, y, z }).name;
			if actual.trim_start_matches("minecraft:") != config.ground_block.trim_start_matches("minecraft:") {
				return Err(format!("地板校验失败：{x} {y} {z} 为 {actual}，预期 {}；请检查控制台中 fill 命令的错误。", config.ground_block));
			}
		}
	}
	Ok(())
}

pub fn world_players(server: &Server) -> Vec<Player> {
	let Some(arena) = world(server) else { return Vec::new(); };
	let id: String = arena.get_id();
	server.get_all_players().into_iter().filter(|player: &Player| player.get_world().get_id() == id).collect()
}

pub fn load_at(server: &Server, player: &Player, pos: Position) {
	if player.get_health() <= 0.0 { player.respawn(); }
	player.set_gamemode(GameMode::Spectator);
	player.set_invulnerable(true);
	player.set_movement_locked(false);
	// Initial login already places players in the lobby and has its own handshake.
	if pos != LOBBY || !in_lobby(player.get_position()) { teleport(server, player, pos); }
}

fn in_lobby(pos: Position) -> bool { (-17.0..18.0).contains(&pos.0) && (-117.0..-82.0).contains(&pos.2) }

fn same_position(first: Position, second: Position) -> bool {
	(first.0 - second.0).abs() < 0.001 && (first.1 - second.1).abs() < 0.001 && (first.2 - second.2).abs() < 0.001
}

fn relocates(player: &Player, power: &Power) -> bool {
	let current: Position = player.get_position();
	match power {
		Power::Lobby | Power::LobbyLoad => !in_lobby(current),
		Power::Load | Power::Spectate => !same_position(current, (0.5, 120.0, 0.5)),
		Power::Start { pos, .. } | Power::Teleport(pos) | Power::Rescue(_, pos) => !same_position(current, *pos),
		Power::Cast(prop) => matches!(prop.as_str(), "warp" | "scroll"),
		_ => false,
	}
}

pub fn unlock_players(server: &Server) {
	for player in world_players(server) { player.set_movement_locked(false); player.set_invulnerable(false); }
}

pub fn unlock_lobby(server: &Server) {
	if !matches!(poll_world(server), Ok(true)) { return; }
	for player in world_players(server) {
		let pos: Position = player.get_position();
		if (-17.0..18.0).contains(&pos.0) && (-117.0..-82.0).contains(&pos.2) {
			player.set_movement_locked(false);
			player.set_invulnerable(false);
		}
	}
}

pub fn prop_id(stack: &ItemStack) -> Option<String> {
	let tree: NbtTree = stack.get_custom_data("jkpof", "prop")?;
	match tree.tags.get(tree.root as usize)? { NbtTag::StringTag(value) => Some(value.clone()), _ => None }
}

pub fn snapshots(server: &Server) -> Vec<Snapshot> {
	let Some(arena) = world(server) else { return Vec::new(); };
	let world_id: String = arena.get_id();
	let mut result: Vec<Snapshot> = Vec::new();
	for player in server.get_all_players() {
		if player.get_world().get_id() != world_id { continue; }
		let pos: Position = player.get_position();
		let (x, y, z): (i32, i32, i32) = floor_position(pos);
		let block: BlockPos = BlockPos { x, y, z };
		let below: String = arena.get_block(block).name;
		let mut charm_slot: Option<u8> = None;
		for slot in 0..if pos.1 < -8.0 { 41_u8 } else { 0_u8 } {
			if let Some(stack) = player.get_inventory_item(slot) {
				if prop_id(&stack).as_deref() == Some("void_charm") { charm_slot = Some(slot); break; }
			}
		}
		result.push(Snapshot {
			id: player.get_id().to_string(), name: player.get_name(), pos,
			health: player.get_health(), max_health: player.get_max_health(),
			below, charm_slot, spectator: player.get_gamemode() == GameMode::Spectator,
		});
	}
	result
}

pub fn command(server: &Server, text: &str) {
	server.execute_command(&format!("execute in {WORLD} run {text}"), CommandSender::Console);
}

pub fn message(player: &Player, text: &str, overlay: bool) {
	if overlay { player.show_actionbar(TextComponent::text(text)); }
	else { player.send_system_message(TextComponent::text(text), false); }
}

fn teleport(server: &Server, player: &Player, pos: Position) {
	player.as_entity().set_velocity((0.0, 0.0, 0.0));
	player.as_entity().set_fall_distance(0.0);
	if same_position(player.get_position(), pos) { return; }
	if let Some(arena) = world(server) { player.teleport(pos, Some(0.0), Some(0.0), arena); }
	player.as_entity().set_fall_distance(0.0);
}

pub fn effect(player: &Player, kind: StatusEffectType, duration: u32, amplifier: u8) {
	player.add_effect(StatusEffectInstance { effect_type: kind, duration, amplifier, ambient: false, show_particles: false, show_icon: true });
}

fn clear_event(player: &Player) {
	for kind in [StatusEffectType::JumpBoost, StatusEffectType::SlowFalling, StatusEffectType::Invisibility] { player.remove_effect(kind); }
	player.as_entity().set_invisible(false);
	if let Some(living) = player.as_entity().as_living() { living.reset_attribute(Attribute::Scale); }
}

fn clean_player(player: &Player) {
	effects::clear_prop_bars(&player.get_id().to_string());
	player.set_movement_locked(false);
	player.set_invulnerable(false);
	player.as_entity().set_invulnerable(false);
	player.clear_effects();
	player.as_entity().set_invisible(false);
	player.as_entity().set_glowing(false);
	player.as_entity().set_fire_ticks(0);
	player.as_entity().set_fall_distance(0.0);
	if let Some(living) = player.as_entity().as_living() {
		for attribute in [Attribute::Scale, Attribute::Gravity] { living.reset_attribute(attribute); }
	}
	player.set_max_health(20.0);
	player.set_health(20.0);
	player.set_food_level(20);
	player.set_saturation(5.0);
	player.get_inventory().clear_all();
}

pub fn release_player(player: &Player) {
	effects::clear_prop_bars(&player.get_id().to_string());
	if player.get_health() <= 0.0 {
		player.set_movement_locked(false);
		player.set_invulnerable(false);
		player.as_entity().set_invulnerable(false);
		return;
	}
	clean_player(player);
	player.set_gamemode(GameMode::Adventure);
}

pub fn contain_entities(server: &Server) {
	let Some(arena) = world(server) else { return; };
	for entity in arena.get_entities() {
		if entity.get_type() == EntityType::Player { continue; }
		let pos: Position = entity.get_position();
		if pos.2 >= -64.0 && (pos.0.abs() > 25.0 || pos.2.abs() > 25.0 || pos.1 > 130.0 || pos.1 < -64.0) { entity.remove(); }
	}
}

fn give_target(player: &Player) -> String {
	// Direct player names use read_string(); selector name= only accepts unquoted
	// strings in this host version. Avoid both selector options and bare UUIDs.
	let name: String = player.get_name().replace('\\', "\\\\").replace('"', "\\\"");
	format!("\"{name}\"")
}

fn give(server: &Server, player: &Player, name: &str, count: u8) {
	let prototype: ItemStack = ItemStack::new(name, 1);
	let max: u8 = prototype.get_max_count().max(1);
	let mut capacity: u32 = 0;
	for slot in 0..36_u8 {
		if let Some(stack) = player.get_inventory_item(slot) {
			if stack.get_registry_key() == name && stack.get_components().is_empty() {
				capacity += u32::from(max.saturating_sub(stack.get_count()));
			}
		} else { capacity += u32::from(max); }
	}
	let accepted: u32 = capacity.min(u32::from(count));
	// /give uses native insertion and inventory synchronization, without the
	// WASM setter's extra packet containing an internal slot as a window slot.
	if accepted > 0 { command(server, &format!("give {} {name} {accepted}", give_target(player))); }
	if accepted < u32::from(count) { message(player, "背包已满，已停止发物。", true); }
}

pub fn give_prop(server: &Server, player: &Player, prop: &str) {
	let (item, title): (&str, &str) = match prop {
		"platform" => ("slime_ball", "急救平台"), "warp" => ("ender_pearl", "折跃珍珠"),
		"star" => ("nether_star", "无敌星"), "scroll" => ("paper", "传送卷轴"),
		"fireball" => ("fire_charge", "可发射的火球"),
		"null_bomb" => ("snowball", "湮灭爆弹"), "cage" => ("glass", "囚笼"),
		"void_charm" => ("totem_of_undying", "虚空护符"), "gravity" => ("magenta_glazed_terracotta", "重力装置"),
		"creative" => ("command_block", "当个创世神"), "tnt" => ("tnt", "可投掷的 TNT"),
		"knockback" => ("stick", "击退棒"), "pickaxe" => ("netherite_pickaxe", "挖掘一切"),
		_ => return,
	};
	if !(0..36_u8).any(|slot: u8| player.get_inventory_item(slot).is_none()) {
		message(player, "背包已满，已停止发物。", true);
		return;
	}
	let lore: &str = match prop {
		"platform" => "主手右键：在脚下生成 5×5 黏液块平台。",
		"warp" => "主手右键：沿视线瞬移最多 20 格，落点限制在比赛场内。",
		"star" => "主手右键：获得 5 秒无敌，掉出场地时救回柱顶，再次使用刷新时长。",
		"scroll" => "主手右键：传送至其他玩家附近或场地中心的安全落点，无落点时返还。",
		"fireball" => "主手右键：沿视线发射一颗火球。",
		"null_bomb" => "主手右键：清空视线前方最多 20 格处的 9×9×9 区域。",
		"cage" => "主手右键：在自身周围生成空心玻璃笼。",
		"void_charm" => "放在背包中：掉到 Y < -8 时自动消耗一个，将你救回初始柱顶。",
		"gravity" => "主手右键：获得漂浮 II 效果 10 秒，再次使用刷新时长。",
		"creative" => "主手右键：获得 3 秒创造模式，结束后恢复生存模式。",
		"tnt" => "主手右键：沿视线投掷 TNT，3 秒后爆炸。",
		_ => "",
	};
	let lore_component: String = if lore.is_empty() { String::new() } else { format!(",lore=[\"{lore}\"]") };
	let enchantments: &str = match prop {
		"knockback" => ",enchantments={knockback:5}",
		"pickaxe" => ",enchantments={efficiency:5,unbreaking:3}",
		_ => "",
	};
	command(server, &format!("give {} minecraft:{item}[custom_name=\"{title}\",custom_data={{jkpof:{{prop:\"{prop}\"}}}}{lore_component}{enchantments}] 1", give_target(player)));
}

fn direction(player: &Player) -> Position {
	let yaw: f64 = f64::from(player.get_yaw()).to_radians();
	let pitch: f64 = f64::from(player.get_pitch()).to_radians();
	(-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos())
}

fn fill(server: &Server, pos: Position, radius: i32, block: &str, hollow: bool) {
	let (x, y, z): (i32, i32, i32) = (pos.0.floor() as i32, pos.1.floor() as i32, pos.2.floor() as i32);
	let (x1, x2): (i32, i32) = ((x - radius).max(-24), (x + radius).min(24));
	let (z1, z2): (i32, i32) = ((z - radius).max(-24), (z + radius).min(24));
	let (y1, y2): (i32, i32) = ((y - radius).max(-64), (y + radius).min(128));
	if x1 <= x2 && y1 <= y2 && z1 <= z2 { command(server, &format!("fill {x1} {y1} {z1} {x2} {y2} {z2} {block} {}", if hollow { "hollow" } else { "strict" })); }
}

fn scroll_landing(arena: &World, anchor: Position) -> Option<Position> {
	let radius: f64 = (arena.get_border().get_diameter() / 2.0 - 1.0).clamp(0.0, 23.0);
	let center_x: i32 = anchor.0.clamp(-radius, radius).floor() as i32;
	let center_z: i32 = anchor.2.clamp(-radius, radius).floor() as i32;
	// Y=129 is the arena ceiling. Both player blocks must remain below it.
	let highest: i32 = (anchor.1.floor() as i32).clamp(-62, 127);
	for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, 1), (1, -1), (-1, -1)] {
		let x: i32 = center_x + dx;
		let z: i32 = center_z + dz;
		if (f64::from(x) + 0.5).abs() > radius || (f64::from(z) + 0.5).abs() > radius { continue; }
		if arena.get_chunk(x.div_euclid(16), z.div_euclid(16)).is_none() { continue; }
		for y in (-62..=highest).rev() {
			let floor: BlockState = arena.get_block_state(BlockPos { x, y: y - 1, z });
			if !floor.up_side_solid || matches!(floor.block_name.trim_start_matches("minecraft:"), "barrier" | "magma_block" | "cactus" | "campfire" | "soul_campfire") { continue; }
			let clear: bool = [y, y + 1].into_iter().all(|height: i32| {
				let name: String = arena.get_block_state(BlockPos { x, y: height, z }).block_name;
				matches!(name.trim_start_matches("minecraft:"), "air" | "cave_air" | "void_air")
			});
			if clear { return Some((f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5)); }
		}
	}
	None
}

fn cast(server: &Server, arena: &World, player: &Player, prop: &str) {
	let pos: Position = player.get_position();
	let dir: Position = direction(player);
	let eye: Position = player.as_entity().get_eye_position();
	match prop {
		"night_vision" => effect(player, StatusEffectType::NightVision, 72000, 0),
		"platform" => {
			let y: i32 = (pos.1 - 1.0).floor() as i32;
			let x: i32 = pos.0.floor() as i32;
			let z: i32 = pos.2.floor() as i32;
			command(server, &format!("fill {} {y} {} {} {y} {} slime_block strict", (x - 2).max(-24), (z - 2).max(-24), (x + 2).min(24), (z + 2).min(24)));
		}
		"warp" => teleport(server, player, ((pos.0 + dir.0 * 20.0).clamp(-23.0, 23.0), (pos.1 + dir.1 * 20.0).clamp(1.0, 126.0), (pos.2 + dir.2 * 20.0).clamp(-23.0, 23.0))),
		"scroll" => {
			let targets: Vec<Player> = server.get_all_players().into_iter().filter(|other: &Player| other.get_world().get_id() == arena.get_id() && other.get_gamemode() == GameMode::Survival && other.get_health() > 0.0 && other.get_id().to_string() != player.get_id().to_string()).collect();
			let seed: usize = uuid::generate().low as usize;
			let anchor: Position = if seed.is_multiple_of(3) || targets.is_empty() { (0.5, 127.0, 0.5) } else { targets[seed % targets.len()].get_position() };
			let target: Option<Position> = scroll_landing(arena, anchor).or_else(|| scroll_landing(arena, pos));
			if let Some(target) = target {
				player.as_entity().set_velocity((0.0, 0.0, 0.0));
				teleport(server, player, target);
			} else {
				give_prop(server, player, "scroll");
				message(player, "附近没有安全落点，传送卷轴已返还。", true);
			}
		}
		"fireball" => {
			let entity: Entity = arena.spawn_entity(EntityType::Fireball, (eye.0 + dir.0 * 2.0, eye.1 + dir.1 * 2.0, eye.2 + dir.2 * 2.0));
			entity.set_velocity((dir.0 * 1.2, dir.1 * 1.2, dir.2 * 1.2));
		}
		"null_bomb" => {
			let end: Position = (eye.0 + dir.0 * 20.0, eye.1 + dir.1 * 20.0, eye.2 + dir.2 * 20.0);
			let hit: Position = arena.ray_trace_blocks(eye, end).unwrap_or(end);
			fill(server, hit, 4, "air", false);
		}
		"cage" => fill(server, (pos.0, pos.1 + 1.0, pos.2), 2, "glass", true),
		"tnt" => command(server, &format!("summon tnt {} {} {} {{fuse:60,Motion:[{}d,{}d,{}d]}}", eye.0 + dir.0 * 2.0, eye.1, eye.2 + dir.2 * 2.0, dir.0, dir.1 * 0.5, dir.2)),
		_ => {}
	}
}

fn event(arena: &World, player: &Player, event: EventKind) {
	let pos: Position = player.get_position();
	match event {
		EventKind::Nothing => clear_event(player),
		EventKind::Moon => { effect(player, StatusEffectType::JumpBoost, 72000, 3); effect(player, StatusEffectType::SlowFalling, 72000, 0); }
		EventKind::Invisible => { effect(player, StatusEffectType::Invisibility, 72000, 0); player.as_entity().set_invisible(true); }
		EventKind::Small => { if let Some(living) = player.as_entity().as_living() { living.set_attribute_base(Attribute::Scale, 0.5); } }
		EventKind::Thunder => {
			arena.strike_lightning(pos, false);
			let offset: u8 = (uuid::generate().low % 36) as u8;
			for index in 0..36_u8 {
				let slot: u8 = (index + offset) % 36;
				if let Some(item) = player.get_inventory_item(slot) { item.add_enchantment(Enchantment::Unbreaking, 3); player.set_inventory_item(slot, Some(item)); break; }
			}
		}
		EventKind::Arrows => {
			for dx in [-2.0, 0.0, 2.0] {
				for dz in [-2.0, 0.0, 2.0] {
					let arrow: Entity = arena.spawn_entity(EntityType::Arrow, ((pos.0 + dx).clamp(-24.0, 24.0), (pos.1 + 12.0).min(127.0), (pos.2 + dz).clamp(-24.0, 24.0)));
					arrow.set_velocity((0.0, -1.5, 0.0));
				}
			}
		}
		EventKind::Iron => effects::spawn_iron(arena, pos),
		_ => {}
	}
}

pub fn execute(server: &Server, actions: Vec<Action>) {
	if actions.is_empty() { return; }
	let Some(arena) = world(server) else { return; };
	let players: HashMap<String, Player> = server.get_all_players().into_iter().filter(|player: &Player| player.get_world().get_id() == arena.get_id()).map(|player: Player| (player.get_id().to_string(), player)).collect();
	for action in actions {
		match action {
			Action::EndEvent => effects::end_event(&arena),
			Action::EventProgress { name, remaining, total } => effects::event_bar(server, &players, &name, remaining, total),
			Action::Announce(text) => arena.broadcast_system_message(TextComponent::text(&format!("[幸运之柱] {text}")), false),
			Action::Command(text) => command(server, &text),
			Action::Border(size) => {
				arena.get_border().set_center(0.0, 0.0);
				effects::border(&arena, size);
			}
			Action::Rules(running, regen) => {
				for (rule, value) in [("pvp", running), ("fall_damage", running), ("fire_damage", running), ("natural_health_regeneration", regen), ("advance_time", false), ("advance_weather", false), ("immediate_respawn", true)] { command(server, &format!("gamerule {rule} {value}")); }
				command(server, if running { "difficulty hard" } else { "difficulty peaceful" });
				command(server, "gamerule respawn_radius 0");
				arena.set_time_of_day(6000);
			}
			Action::Magnet(ids) => {
				for entity in arena.get_entities() {
					if entity.get_type() != EntityType::Item { continue; }
					let pos: Position = entity.get_position();
					for id in &ids {
						if let Some(player) = players.get(id) {
							let target: Position = player.get_position();
							let delta: Position = (target.0 - pos.0, target.1 + 0.5 - pos.1, target.2 - pos.2);
							if delta.0 * delta.0 + delta.1 * delta.1 + delta.2 * delta.2 < 36.0 { entity.set_velocity((delta.0 * 0.2, delta.1 * 0.2, delta.2 * 0.2)); break; }
						}
					}
				}
			}
			Action::Player(id, power) => {
				let Some(player) = players.get(&id) else { continue; };
				if matches!(power, Power::Lobby | Power::LobbyLoad | Power::Load | Power::Start { .. } | Power::Spectate) { crate::transfer::cancel(&id); }
				let java: bool = player.as_java().is_some();
				let respawning: bool = java && player.get_health() <= 0.0 && matches!(power, Power::Lobby | Power::LobbyLoad | Power::Load | Power::Start { .. } | Power::Spectate);
				if respawning { crate::transfer::requested(&id); player.respawn(); }
				let power: Power = if java && (respawning || relocates(player, &power)) {
					let Some(power) = crate::transfer::defer(&id, power) else { continue; };
					power
				} else { power };
				match power {
					Power::Lobby => {
						if player.get_health() <= 0.0 { player.respawn(); }
						clean_player(player);
						player.set_gamemode(GameMode::Adventure);
						if !in_lobby(player.get_position()) { teleport(server, player, LOBBY); }
						if let Some(team) = player.get_team() { if team.starts_with("pof_") { arena.get_scoreboard().remove_player_from_team(&team, &player.get_name()); } }
						message(player, "请右键大厅的准备面板确认参赛；/pof 查看帮助。", false);
					}
					Power::Load => load_at(server, player, (0.5, 120.0, 0.5)),
					Power::LobbyLoad => load_at(server, player, LOBBY),
					Power::Start { pos, health, team, cage } => {
						clean_player(player); player.set_max_health(health); player.set_health(health);
						player.set_gamemode(GameMode::Survival); player.set_invulnerable(true); player.set_movement_locked(false);
						teleport(server, player, pos);
						assign_team(&arena, player, team);
						if cage {
							let x: i32 = pos.0.floor() as i32;
							let y: i32 = pos.1.floor() as i32;
							let z: i32 = pos.2.floor() as i32;
							command(server, &format!("fill {} {y} {} {} {} {} glass hollow", x - 1, z - 1, x + 1, y + 2, z + 1));
							command(server, &format!("fill {x} {y} {z} {x} {} {z} air strict", y + 1));
						}
						message(player, "开局保护中……", true);
					}
					Power::Unfreeze => {
						let pos: Position = player.get_position();
						let (x, y, z): (i32, i32, i32) = (pos.0.floor() as i32, pos.1.floor() as i32, pos.2.floor() as i32);
						command(server, &format!("fill {} {y} {} {} {} {} air replace glass", x - 1, z - 1, x + 1, y + 2, z + 1));
						player.set_movement_locked(false); player.set_invulnerable(false);
					}
					Power::Spectate => {
						if player.get_health() <= 0.0 { player.respawn(); }
						clean_player(player); player.set_gamemode(GameMode::Spectator); teleport(server, player, (0.5, 120.0, 0.5));
					}
					Power::Teleport(pos) => teleport(server, player, pos),
					Power::Heal => player.set_health(player.get_max_health()),
					Power::Give(item, count) => give(server, player, &item, count),
					Power::Prop(prop) => give_prop(server, player, &prop),
					Power::Rescue(slot, pos) => {
						if let Some(item) = player.get_inventory_item(slot) {
							if prop_id(&item).as_deref() == Some("void_charm") {
								let count: u8 = item.get_count();
								if count <= 1 { player.set_inventory_item(slot, None); } else { item.set_count(count - 1); player.set_inventory_item(slot, Some(item)); }
								teleport(server, player, pos); message(player, "虚空护符已生效。", true);
							}
						}
					}
					Power::Invulnerable(value) => player.set_invulnerable(value),
					Power::Star(active) => {
						player.set_invulnerable(active);
						player.as_entity().set_invulnerable(active);
						effects::prop_bar(server, player, "star", if active { STAR_DURATION_TICKS } else { 0 });
					}
					Power::Creative(active) => {
						player.set_gamemode(if active { GameMode::Creative } else { GameMode::Survival });
						effects::prop_bar(server, player, "creative", if active { CREATIVE_DURATION_TICKS } else { 0 });
					}
					Power::Gravity(active) => {
						if active { effect(player, StatusEffectType::Levitation, 200, 1); }
						else { player.remove_effect(StatusEffectType::Levitation); }
						effects::prop_bar(server, player, "gravity", if active { 200 } else { 0 });
					}
					Power::PropProgress { prop, remaining } => effects::prop_bar(server, player, prop, remaining),
					Power::Event(kind) => event(&arena, player, kind),
					Power::Cast(prop) => cast(server, &arena, player, &prop),
					Power::Ui(text) => {
						let text: String = if text.starts_with("存活 ") && !(0..36_u8).any(|slot: u8| player.get_inventory_item(slot).is_none()) {
							text.split(" | ").map(|part: &str| if part.starts_with("发物 ") { "背包已满，停止发物" } else { part }).collect::<Vec<&str>>().join(" | ")
						} else { text };
						message(player, &text, true);
					}
					Power::EventTitle(kind) => {
						player.send_title_animation(0, 40, 0);
						player.show_subtitle(TextComponent::text(kind.description()));
						player.show_title(TextComponent::text(kind.name()));
					}
				}
			}
		}
	}
}

fn assign_team(arena: &World, player: &Player, index: usize) {
	let name: String = format!("pof_{index}");
	let scoreboard: pumpkin_plugin_api::scoreboard::Scoreboard = arena.get_scoreboard();
	if scoreboard.get_team(&name).is_none() { scoreboard.create_team(&name, TeamSettingsBuilder::new().friendly_fire(false).build()); }
	scoreboard.add_player_to_team(&name, &player.get_name());
}
