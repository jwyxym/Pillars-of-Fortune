use crate::{config::{Config, EventKind, GROUND_MATERIALS, PILLAR_MATERIALS, RANDOM_MATERIAL}, game::{Phase, Position}, runtime};
use pumpkin_plugin_api::{
	BillboardMode, Entity, EntityDisplayExt, EntityType, ItemDisplayMode, ItemStack, Player,
	Server, TextDisplayEntityExt, TransformationBuilder, World,
	persistent_data::string_tree,
	world::Block,
};
use serde_json::Value;
use std::{collections::{HashMap, HashSet}, time::{Duration, Instant}};

const NAMESPACE: &str = "pillars-of-fortune";
const MARKER: &str = "lobby_display";
const EVENTS_TITLE: &str = if cfg!(feature = "admin-settings") { "事件设置 · 管理员右键切换" } else { "事件设置 · 右键切换" };
const SETTINGS_TITLE: &str = if cfg!(feature = "admin-settings") {
	"游戏设置 · 管理员右键切换\n上：发物与道具　中：场地　下：比赛规则"
} else {
	"游戏设置 · 右键切换\n上：发物与道具　中：场地　下：比赛规则"
};

struct Card {
	position: Position,
	text: String,
	item: String,
	command: Option<String>,
}

impl Card {
	fn is_material(&self) -> bool {
		self.command.as_ref().is_some_and(|command: &String| command.starts_with("set pillar_block ") || command.starts_with("set ground_block "))
	}
}

fn update_icon(entity: &Entity, card: &Card) {
	let name: String = format!("minecraft:{}", card.item);
	if card.is_material() {
		if let (Some(display), Some(block)) = (entity.as_block_display(), Block::from_name(&name)) {
			display.set_block_state_id(block.default_state_id);
		}
	} else if let Some(display) = entity.as_item_display() {
		display.set_item(Some(ItemStack::new(&name, 1)));
	}
}

struct Entry {
	text_id: u32,
	icon_id: u32,
	button_id: Option<u32>,
	card: Card,
}

#[derive(Default)]
pub struct Lobby {
	entries: Vec<Entry>,
	owned: HashSet<u32>,
	clicks: HashMap<(String, u32), Instant>,
	ticks: u8,
}

pub fn cleanup(server: &Server) {
	if let Some(world) = runtime::world(server) {
		for entity in world.get_entities() {
			if entity.has_custom_data(NAMESPACE, MARKER) { entity.remove(); }
		}
	}
}

fn spawn(world: &World, kind: EntityType, position: Position) -> Entity {
	let entity: Entity = world.spawn_entity(kind, position);
	entity.set_custom_data(NAMESPACE, MARKER, &string_tree("v1"));
	entity
}

fn text(world: &World, position: Position, value: &str, scale: f32) -> Entity {
	let entity: Entity = spawn(world, EntityType::TextDisplay, position);
	entity.set_rotation(facing(position), 0.0);
	if let Some(display) = entity.as_text_display() {
		display.set_plain_text(value);
		display.set_line_width(150);
		display.set_shadow(true);
		display.set_background(0x50000000);
		display.set_text_opacity(-1);
		display.get_display().set_billboard(BillboardMode::Fixed);
		display.get_display().set_transformation(TransformationBuilder::new().uniform_scale(scale).build());
	}
	entity
}

// Four upright panels face the central walkway. Every card stays in its panel's plane.
fn facing(position: Position) -> f32 {
	if position.0 == -10.0 { -90.0 }
	else if position.0 == 10.0 { 90.0 }
	else if position.2 == -90.0 { 180.0 }
	else { 0.0 }
}

fn grid(center: Position, columns: usize, rows: usize, index: usize, spacing: f64, row_height: f64, sideways: bool) -> Position {
	let offset: f64 = (index % columns) as f64 * spacing - (columns - 1) as f64 * spacing / 2.0;
	let y: f64 = center.1 + (rows - 1 - index / columns) as f64 * row_height;
	if sideways { (center.0, y, center.2 + offset) } else { (center.0 + offset, y, center.2) }
}

fn setting_position(key: &str) -> Position {
	let order: [&str; 19] = [
		"item_interval", "item_count", "prop_chance", "same_items", "fishing", "bonus_prop", "initial_tool",
		"pillar_height", "pillar_radius", "pillar_block", "ground_radius", "ground_layers", "ground_block", "spawn_protection",
		"event_interval", "teams", "double_health", "night_vision", "natural_regeneration",
	];
	let index: usize = order.iter().position(|name: &&str| *name == key).expect("known lobby setting");
	grid((0.0, 64.6, -110.0), 7, 3, index, 1.8, 1.2, false)
}

impl Lobby {
	pub fn tick(&mut self, server: &Server, config: &Config, phase: Phase, players: usize, ready: usize, force: bool) {
		self.ticks = self.ticks.wrapping_add(1);
		if (!force && self.ticks % 4 != 1) || !matches!(runtime::poll_world(server), Ok(true)) { return; }
		let Some(world) = runtime::world(server) else { return; };
		let entities: HashMap<u32, Entity> = world.get_entities().into_iter().map(|entity: Entity| (entity.get_id(), entity)).collect();
		let present: HashSet<u32> = entities.keys().copied().collect();
		let cards: Vec<Card> = cards(config, phase, players, ready);
		if self.entries.len() != cards.len() || !self.owned.is_subset(&present) {
			cleanup(server);
			self.entries.clear();
			self.owned.clear();
			for (position, title) in [
				((0.0, 68.4, -90.0), "幸运之柱 · 准备与游戏"),
				((-10.0, 68.4, -100.0), "道具图鉴"),
				((10.0, 68.4, -100.0), EVENTS_TITLE),
				((0.0, 68.4, -110.0), SETTINGS_TITLE),
			] {
				let entity: Entity = text(&world, position, title, 0.9);
				self.owned.insert(entity.get_id());
			}
			for card in cards {
				let (x, y, z): Position = card.position;
				let label: Entity = text(&world, (x, y + 0.35, z), &card.text, 0.38);
				self.owned.insert(label.get_id());
				let kind: EntityType = if card.is_material() { EntityType::BlockDisplay } else { EntityType::ItemDisplay };
				let icon: Entity = spawn(&world, kind, card.position);
				icon.set_rotation(facing(card.position), 0.0);
				self.owned.insert(icon.get_id());
				update_icon(&icon, &card);
				if let Some(display) = icon.as_item_display() {
					display.set_item_display_mode(ItemDisplayMode::Gui);
				}
				if let Some(display) = icon.as_display() {
					let transform: TransformationBuilder = if card.is_material() {
						TransformationBuilder::new().translation(-0.2, -0.2, -0.2)
					} else { TransformationBuilder::new() };
					display.set_billboard(BillboardMode::Fixed);
					display.set_transformation(transform.uniform_scale(0.4).build());
				}
				let button_id: Option<u32> = if card.command.is_some() {
					let button: Entity = spawn(&world, EntityType::Interaction, (x, y - 0.3, z));
					if let Some(interaction) = button.as_interaction() {
						interaction.set_width(0.72);
						interaction.set_height(0.85);
						interaction.set_response(true);
					}
					self.owned.insert(button.get_id());
					Some(button.get_id())
				} else { None };
				self.entries.push(Entry { text_id: label.get_id(), icon_id: icon.get_id(), button_id, card });
			}
			tracing::info!("大厅文字、道具展示和设置按钮已生成");
		} else {
			for (entry, card) in self.entries.iter_mut().zip(cards) {
				if entry.card.item != card.item {
					if let Some(entity) = entities.get(&entry.icon_id) {
						update_icon(entity, &card);
					}
				}
				if entry.card.text != card.text {
					if let Some(entity) = entities.get(&entry.text_id) {
						if let Some(display) = entity.as_text_display() { display.set_plain_text(&card.text); }
					}
				}
				entry.card = card;
			}
		}
		let online: HashSet<String> = runtime::world_players(server).iter().map(|player: &Player| player.get_id().to_string()).collect();
		self.clicks.retain(|(id, _): &(String, u32), _: &mut Instant| online.contains(id));
	}

	// This method only touches plain state. Host calls happen after releasing the lobby lock.
	pub fn click(&mut self, entity_id: u32, id: &str, position: Position) -> Option<String> {
		let entry: &Entry = self.entries.iter().find(|entry: &&Entry| entry.button_id == Some(entity_id))?;
		let (x, y, z): Position = entry.card.position;
		if (position.0 - x).powi(2) + (position.1 + 1.6 - y).powi(2) + (position.2 - z).powi(2) > 25.0 { return None; }
		let now: Instant = Instant::now();
		let key: (String, u32) = (id.to_string(), entity_id);
		if self.clicks.get(&key).is_some_and(|last: &Instant| now.duration_since(*last) < Duration::from_millis(200)) { return None; }
		self.clicks.insert(key, now);
		entry.card.command.clone()
	}

	pub fn owns(&self, entity_id: u32) -> bool { self.owned.contains(&entity_id) }
}

fn setting(config: &Value, key: &str, label: &str, item: &'static str, options: &[Value], position: Position) -> Card {
	let current: &Value = &config[key];
	let next: usize = options.iter().position(|value: &Value| value == current).map_or(0, |index: usize| (index + 1) % options.len());
	let value: String = match current { Value::Bool(true) => "开启".to_string(), Value::Bool(false) => "关闭".to_string(), Value::String(value) if value == RANDOM_MATERIAL => "随机".to_string(), Value::String(value) => value.trim_start_matches("minecraft:").to_string(), _ => current.to_string() };
	let icon: String = if matches!(key, "pillar_block" | "ground_block") {
		if current.as_str() == Some(RANDOM_MATERIAL) { "jigsaw".to_string() }
		else { current.as_str().unwrap_or(item).trim_start_matches("minecraft:").to_string() }
	} else { item.to_string() };
	Card { position, text: format!("{label}\n{value}"), item: icon, command: Some(format!("set {key} {}", options[next])) }
}

pub fn phase_name(phase: Phase) -> &'static str {
	match phase { Phase::Lobby => "等待点击开始", Phase::Building => "场地生成中", Phase::Running => "比赛进行中", Phase::Ending => "比赛结算中" }
}

fn material_options(materials: &[&str]) -> Vec<Value> {
	materials.iter().copied().chain(std::iter::once(RANDOM_MATERIAL)).map(|material: &str| Value::from(material)).collect()
}

fn cards(config: &Config, phase: Phase, players: usize, ready: usize) -> Vec<Card> {
	let state: &str = phase_name(phase);
	let mut result: Vec<Card> = vec![
		Card { position: (-4.0, 65.0, -86.0), text: "准备参赛\n右键确认准备".to_string(), item: "emerald".to_string(), command: Some("join".to_string()) },
		Card { position: (0.0, 65.0, -86.0), text: format!("在线 {players} 人\n已准备 {ready} 人\n{state}\n右键查看状态"), item: "nether_star".to_string(), command: Some("status".to_string()) },
		Card { position: (4.0, 65.0, -86.0), text: "取消准备 / 退出\n右键 /pof leave".to_string(), item: "red_concrete".to_string(), command: Some("leave".to_string()) },
		Card { position: (-4.0, 65.0, -90.0), text: "开始游戏\n准备完成后右键开局".to_string(), item: "diamond_sword".to_string(), command: Some("start".to_string()) },
	];
	for (index, card) in result.iter_mut().enumerate() {
		card.position = if index < 3 {
			grid((0.0, 66.4, -90.0), 3, 1, index, 2.2, 1.2, false)
		} else { (0.0, 65.2, -90.0) };
	}
	let json: Value = serde_json::to_value(config).unwrap_or(Value::Null);
	let settings: Vec<(&str, &str, &str, Vec<Value>)> = vec![
		("item_interval", "发物间隔 / 秒", "clock", vec![1.into(), 3.into(), 5.into(), 10.into(), 15.into(), 30.into(), 60.into()]),
		("item_count", "每轮物品数量", "chest", vec![1.into(), 2.into(), 4.into(), 8.into(), 16.into(), 32.into(), 64.into()]),
		("prop_chance", "道具概率 / %", "nether_star", vec![0.into(), 3.into(), 5.into(), 10.into(), 25.into(), 50.into(), 100.into()]),
		("pillar_height", "柱子高度", "stone", vec![10.into(), 30.into(), 60.into(), 80.into(), 100.into()]),
		("pillar_radius", "柱子分布半径", "end_rod", vec![8.into(), 12.into(), 16.into(), 20.into()]),
		("ground_radius", "地面半径", "bedrock", vec![12.into(), 14.into(), 16.into(), 18.into(), 20.into(), 22.into()]),
		("ground_layers", "地面层数", "stone_bricks", vec![1.into(), 2.into(), 3.into(), 4.into(), 5.into()]),
		("event_interval", "事件间隔 / 秒", "bell", vec![30.into(), 60.into(), 90.into(), 120.into(), 180.into(), 300.into()]),
		("teams", "队伍数 / 0为单人", "white_banner", vec![0.into(), 2.into(), 3.into(), 4.into(), 6.into(), 12.into()]),
		("pillar_block", "柱子材质", "cobblestone", material_options(&PILLAR_MATERIALS)),
		("ground_block", "地面材质", "grass_block", material_options(&GROUND_MATERIALS)),
	];
	for (key, label, item, options) in settings {
		let position: Position = setting_position(key);
		result.push(setting(&json, key, label, item, &options, position));
	}
	for (key, label, item) in [
		("same_items", "全员相同物品", "ender_chest"), ("double_health", "双倍生命", "cake"),
		("night_vision", "夜视", "golden_carrot"), ("natural_regeneration", "自然回血", "golden_apple"),
		("fishing", "钓鱼获得物品", "fishing_rod"),
		("initial_tool", "开局工具", "netherite_pickaxe"), ("bonus_prop", "开局道具", "chest"),
		("spawn_protection", "开局玻璃笼", "glass"),
	] {
		let position: Position = setting_position(key);
		result.push(setting(&json, key, label, item, &[false.into(), true.into()], position));
	}
	for (index, event) in EventKind::ALL.iter().copied().enumerate() {
		let enabled: bool = config.events.contains(&event);
		let mut events: Vec<EventKind> = config.events.clone();
		if enabled { events.retain(|value: &EventKind| *value != event); } else { events.push(event); }
		result.push(Card {
			position: grid((10.0, 64.6, -100.0), 6, 3, index, 1.5, 1.2, true),
			text: format!("{}\n{}", event.name(), if enabled { "开启" } else { "关闭" }),
			item: if enabled { "lime_dye" } else { "gray_dye" }.to_string(),
			command: Some(format!("set events {}", serde_json::to_string(&events).unwrap_or_default())),
		});
	}
	for (index, (item, title, description)) in [
		("slime_ball", "急救平台", "脚下生成平台"), ("ender_pearl", "折跃珍珠", "向前传送"),
		("nether_star", "无敌星", "无敌 5 秒 · 掉出场地救回"), ("paper", "传送卷轴", "随机传送至场内位置"),
		("fire_charge", "火球", "右键发射火球"),
		("snowball", "湮灭爆弹", "清空瞄准位置方块"), ("glass", "囚笼", "在自身周围生成玻璃笼"),
		("totem_of_undying", "虚空护符", "背包携带 · 坠落救回"), ("magenta_glazed_terracotta", "重力装置", "漂浮 II 持续 10 秒 · 再次使用刷新"),
		("command_block", "当个创世神", "创造模式持续 3 秒"), ("tnt", "可投掷的TNT", "右键投掷TNT"),
		("stick", "击退棒", "击退附魔近战武器"), ("netherite_pickaxe", "挖掘一切", "高效率镐"),
	].into_iter().enumerate() {
		result.push(Card { position: grid((-10.0, 64.6, -100.0), 4, 4, index, 1.8, 0.95, true), text: format!("{title}\n{description}"), item: item.to_string(), command: None });
	}
	result
}
