use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub const RANDOM_MATERIAL: &str = "random";
pub const PILLAR_MATERIALS: [&str; 4] = ["minecraft:stone", "minecraft:glass", "minecraft:oak_planks", "minecraft:obsidian"];
pub const GROUND_MATERIALS: [&str; 4] = ["minecraft:bedrock", "minecraft:stone", "minecraft:glass", "minecraft:slime_block"];

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
	Nothing, Border, Arrows, Lava, Thunder, Moon, Swap, Supply,
	Invisible, Harvest, Iron, Magnet, Small, Touch, Purge, Nuke,
}

impl EventKind {
	pub const ALL: &[Self] = &[
		Self::Nothing, Self::Border, Self::Arrows, Self::Lava, Self::Thunder,
		Self::Moon, Self::Swap, Self::Supply, Self::Invisible,
		Self::Harvest, Self::Iron, Self::Magnet,
		Self::Small,
		Self::Touch, Self::Purge, Self::Nuke,
	];

	pub fn name(self) -> &'static str {
		match self {
			Self::Nothing => "无事发生", Self::Border => "边界收缩",
			Self::Arrows => "万箭齐发", Self::Lava => "熔岩地板",
			Self::Thunder => "雷霆之劫", Self::Moon => "月球漫步",
			Self::Swap => "连锁换位", Self::Supply => "道具补给",
			Self::Invisible => "隐形涂层",
			Self::Harvest => "五谷丰登",
			Self::Iron => "钢铁洪流", Self::Magnet => "吸星大法",
			Self::Small => "小孩模式", Self::Touch => "触则生变",
			Self::Purge => "万物归零", Self::Nuke => "世界核平",
		}
	}

	pub fn description(self) -> &'static str {
		match self {
			Self::Nothing => "风平浪静，趁机整理装备。",
			Self::Border => "安全区逐渐缩小，请向场地中心移动。",
			Self::Arrows => "箭雨从头顶落下，注意躲避！",
			Self::Lava => "踩过的方块逐渐化为熔岩，别停留！",
			Self::Thunder => "随机一人遭雷击，并获得一件物品的耐久附魔。",
			Self::Moon => "跳得更高，落得更慢。",
			Self::Swap => "存活玩家互换位置，小心脚下！",
			Self::Supply => "接下来发物时，更容易获得道具。",
			Self::Invisible => "所有存活玩家隐形，留意周围动静。",
			Self::Harvest => "未满血则回满血，满血则获得随机道具。",
			Self::Iron => "铁块从头顶落下，注意躲避！",
			Self::Magnet => "随机一人获得吸取附近掉落物的能力。",
			Self::Small => "身体缩小一半，灵活穿梭战场。",
			Self::Touch => "踩过的方块随机变换材质。",
			Self::Purge => "清除比赛区域内所有非玩家实体。",
			Self::Nuke => "场地的一个象限将被清空，注意脚下！",
		}
	}
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
	pub item_interval: u32,
	pub item_count: u8,
	pub same_items: bool,
	pub prop_chance: u8,
	pub pillar_height: i32,
	pub pillar_radius: i32,
	pub ground_radius: i32,
	pub ground_layers: i32,
	pub pillar_block: String,
	pub ground_block: String,
	pub countdown: u32,
	pub event_interval: u32,
	pub teams: u8,
	pub double_health: bool,
	pub night_vision: bool,
	pub natural_regeneration: bool,
	pub fishing: bool,
	pub initial_tool: bool,
	pub bonus_prop: bool,
	pub spawn_protection: bool,
	pub events: Vec<EventKind>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			item_interval: 5, item_count: 1, same_items: false, prop_chance: 3,
			pillar_height: 60, pillar_radius: 16, ground_radius: 16, ground_layers: 1,
			pillar_block: "minecraft:stone".to_string(), ground_block: "minecraft:bedrock".to_string(),
			countdown: 5, event_interval: 60, teams: 0, double_health: false,
			night_vision: true, natural_regeneration: true, fishing: false,
			initial_tool: false, bonus_prop: false, spawn_protection: true,
			events: EventKind::ALL.to_vec(),
		}
	}
}

impl Config {
	pub fn validate(&self) -> Result<(), String> {
		if !(1..=60).contains(&self.item_interval) || !(1..=64).contains(&self.item_count)
			|| self.prop_chance > 100 || !(10..=100).contains(&self.pillar_height)
			|| !(8..=20).contains(&self.pillar_radius) || ![12, 14, 16, 18, 20, 22].contains(&self.ground_radius)
			|| !(1..=5).contains(&self.ground_layers) || !(3..=30).contains(&self.countdown)
			|| !(30..=300).contains(&self.event_interval) || self.teams == 1 || self.teams > 12 {
			return Err("配置范围错误，请参阅 README 的配置表。".to_string());
		}
		if self.events.is_empty() { return Err("events 至少需要保留一个事件。".to_string()); }
		for block in [&self.pillar_block, &self.ground_block] {
			if block == RANDOM_MATERIAL { continue; }
			if !block.starts_with("minecraft:") || !block.bytes().all(|value: u8| value.is_ascii_lowercase() || value.is_ascii_digit() || b"_:".contains(&value)) {
				return Err("材质必须是 minecraft: 命名空间的方块 ID，或 random（随机）。".to_string());
			}
		}
		Ok(())
	}

	pub fn load(path: &Path) -> Result<Self, String> {
		if !path.exists() {
			let config: Self = Self::default();
			config.save(path)?;
			return Ok(config);
		}
		let text: String = fs::read_to_string(path).map_err(|error: std::io::Error| error.to_string())?;
		let mut json: serde_json::Value = serde_json::from_str(&text).map_err(|error: serde_json::Error| error.to_string())?;
		if let Some(object) = json.as_object_mut() { object.remove("voting"); }
		if let Some(events) = json.get_mut("events").and_then(serde_json::Value::as_array_mut) {
			let removed: bool = events.iter().any(|event: &serde_json::Value| matches!(event.as_str(), Some("betrayal" | "lock")));
			events.retain(|event: &serde_json::Value| !matches!(event.as_str(), Some("betrayal" | "lock")));
			if removed && events.is_empty() { events.push(serde_json::Value::String("nothing".to_string())); }
		}
		let config: Self = serde_json::from_value(json).map_err(|error: serde_json::Error| error.to_string())?;
		config.validate()?;
		Ok(config)
	}

	pub fn save(&self, path: &Path) -> Result<(), String> {
		self.validate()?;
		let text: String = serde_json::to_string_pretty(self).map_err(|error: serde_json::Error| error.to_string())?;
		fs::write(path, text).map_err(|error: std::io::Error| error.to_string())
	}
}
