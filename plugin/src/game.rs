use crate::config::{Config, EventKind, GROUND_MATERIALS, PILLAR_MATERIALS, RANDOM_MATERIAL};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

pub type Position = (f64, f64, f64);
const EVENT_WARNING_TICKS: u64 = 40;
const EVENT_DURATION_TICKS: u64 = 200;
pub const STAR_DURATION_TICKS: u64 = 100;
pub const CREATIVE_DURATION_TICKS: u64 = 60;

pub fn floor_position(pos: Position) -> (i32, i32, i32) {
	(pos.0.floor() as i32, (pos.1 - 0.05).floor() as i32, pos.2.floor() as i32)
}
pub const LOBBY: Position = (0.5, 64.0, -99.5);
pub const PROPS: [&str; 13] = [
	"platform", "warp", "star", "scroll", "fireball",
	"null_bomb", "cage", "void_charm", "gravity", "creative", "tnt", "knockback", "pickaxe",
];

#[derive(Clone)]
pub struct Snapshot {
	pub id: String,
	pub name: String,
	pub pos: Position,
	pub health: f32,
	pub max_health: f32,
	pub below: String,
	pub charm_slot: Option<u8>,
	pub spectator: bool,
}

impl Snapshot {
	fn has_event_floor(&self) -> bool {
		!matches!(self.below.trim_start_matches("minecraft:"), "air" | "cave_air" | "void_air" | "water" | "lava" | "barrier")
	}
}

struct MeltingBlock { started: u64, original: String }

pub enum Power {
	Lobby,
	LobbyLoad,
	Load,
	Start { pos: Position, health: f32, team: usize, cage: bool },
	Spectate,
	Teleport(Position),
	Unfreeze,
	Heal,
	Give(String, u8),
	Prop(String),
	Rescue(u8, Position),
	Invulnerable(bool),
	Star(bool),
	Creative(bool),
	Gravity(bool),
	PropProgress { prop: &'static str, remaining: u64 },
	Event(EventKind),
	Cast(String),
	Ui(String),
	EventTitle(EventKind),
}

pub enum Action {
	EndEvent,
	EventProgress { name: String, remaining: u64, total: u64 },
	Announce(String),
	Player(String, Power),
	Command(String),
	Border(f64),
	Rules(bool, bool),
	Magnet(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Lobby, Building, Running, Ending }

pub struct Member {
	pub name: String,
	pub team: usize,
	pub spawn: Position,
	pub alive: bool,
	pub spectated: bool,
	pub star_until: u64,
	pub creative_until: u64,
	pub rescue_until: u64,
	pub gravity: bool,
	pub gravity_until: u64,
	pub cooldown: u64,
}

pub struct Game {
	pub config: Config,
	round_config: Config,
	pub phase: Phase,
	pub members: HashMap<String, Member>,
	pub clock: u64,
	pub active: bool,
	pub task: Option<u32>,
	pub clean_task: Option<u32>,
	clean_y: Option<i32>,
	clean_busy: bool,
	clean_epoch: u64,
	manual_ready: HashSet<String>,
	seen: HashSet<String>,
	remaining: u64,
	round_time: u64,
	item_time: u64,
	event_time: u64,
	event: EventKind,
	pending_event: Option<(EventKind, u64)>,
	event_ends: Option<u64>,
	magnet_target: Option<String>,
	supply: u8,
	shrinks: u8,
	nuked: HashSet<usize>,
	melting: HashMap<(i32, i32, i32), MeltingBlock>,
	build: VecDeque<Action>,
	loading_ticks: u64,
	items: Vec<String>,
	random: u64,
	test_mode: bool,
}

impl Game {
	pub fn new(config: Config, items: Vec<String>, seed: u64) -> Self {
		let round_config: Config = config.clone();
		Self {
			config, round_config, phase: Phase::Lobby, members: HashMap::new(), clock: 0, active: true, task: None,
			clean_task: None, clean_y: Some(129), clean_busy: false, clean_epoch: 0,
			manual_ready: HashSet::new(), seen: HashSet::new(), remaining: 0,
			round_time: 0, item_time: 0, event_time: 0, event: EventKind::Nothing, pending_event: None, event_ends: None,
			magnet_target: None, supply: 0, shrinks: 0,
			nuked: HashSet::new(), melting: HashMap::new(), build: VecDeque::new(), loading_ticks: 0, items,
			random: seed.max(1), test_mode: false,
		}
	}

	pub fn random(&mut self, limit: usize) -> usize {
		self.random ^= self.random << 13;
		self.random ^= self.random >> 7;
		self.random ^= self.random << 17;
		(self.random % limit.max(1) as u64) as usize
	}

	pub fn status(&self) -> String {
		format!("幸运之柱：{:?}，存活 {}/{}；发物 {} 秒 × {}，柱高 {}，队伍 {}，事件 {}。",
			self.phase, self.members.values().filter(|member: &&Member| member.alive).count(), self.members.len(),
			self.config.item_interval, self.config.item_count, self.config.pillar_height, self.config.teams, self.event_label())
	}

	fn event_label(&self) -> String {
		if let Some((event, starts)) = self.pending_event {
			format!("{} · {}s后生效", event.name(), starts.saturating_sub(self.clock).div_ceil(20))
		} else if let Some(ends) = self.event_ends {
			format!("{} · 剩余{}s", self.event.name(), ends.saturating_sub(self.clock).div_ceil(20))
		} else { "等待下个事件".to_string() }
	}

	pub fn ready_count(&self) -> usize { self.manual_ready.len() }

	pub fn ready(&mut self, id: &str, ready: bool) -> Result<(), String> {
		if self.phase != Phase::Lobby { return Err("比赛已经开始，请等待下一局。".to_string()); }
		if ready { self.manual_ready.insert(id.to_string()); } else { self.manual_ready.remove(id); }
		Ok(())
	}

	pub fn mark_dead(&mut self, id: &str) {
		self.manual_ready.remove(id);
		if let Some(member) = self.members.get_mut(id) { member.alive = false; }
	}

	pub fn is_alive(&self, id: &str) -> bool {
		self.phase == Phase::Running && self.members.get(id).is_some_and(|member: &Member| member.alive)
	}

	pub fn has_star(&self, id: &str) -> bool {
		self.is_alive(id) && self.members.get(id).is_some_and(|member: &Member| member.star_until > self.clock)
	}

	pub fn has_rescue_protection(&self, id: &str) -> bool {
		self.is_alive(id) && self.members.get(id).is_some_and(|member: &Member| member.rescue_until > self.clock)
	}

	pub fn reset(&mut self, players: &[Snapshot]) -> Vec<Action> {
		self.phase = Phase::Lobby;
		self.members.clear();
		self.manual_ready.clear();
		self.build.clear();
		self.loading_ticks = 0;
		self.clean_y = Some(129);
		self.clean_busy = false;
		self.clean_epoch = self.clean_epoch.wrapping_add(1);
		self.melting.clear();
		self.magnet_target = None;
		self.event = EventKind::Nothing;
		self.pending_event = None;
		self.event_ends = None;
		self.supply = 0;
		self.seen.extend(players.iter().map(|player: &Snapshot| player.id.clone()));
		let mut actions: Vec<Action> = vec![Action::EndEvent];
		actions.extend(players.iter().map(|player: &Snapshot| Action::Player(player.id.clone(), Power::Lobby)));
		actions.extend([Action::Border(1000.0), Action::Rules(false, self.config.natural_regeneration), Action::Announce("已返回大厅。请右键准备面板确认参赛。".to_string())]);
		actions
	}

	pub fn start(&mut self, players: &[Snapshot], test_mode: bool) -> Result<Vec<Action>, String> {
		if self.phase != Phase::Lobby { return Err("请先结束当前比赛。".to_string()); }
		let candidates: Vec<Snapshot> = players.iter().filter(|player: &&Snapshot| self.manual_ready.contains(&player.id)).cloned().collect();
		let selected: &[Snapshot] = if test_mode && candidates.is_empty() { players } else { &candidates };
		if selected.is_empty() || (!test_mode && selected.len() < 2) || selected.len() > 12 {
			return Err("需要 2～12 名已准备玩家；/pof test 可以单人测试。".to_string());
		}
		if self.config.teams > 0 && selected.len() < usize::from(self.config.teams) { return Err("参赛人数不能小于队伍数。".to_string()); }
		// Preserve the original template's exact outline, but place blocks through fill.
		let floors: HashMap<String, Vec<[i32; 4]>> = serde_json::from_str(include_str!("../data/grounds.json"))
			.map_err(|error: serde_json::Error| format!("地板轮廓读取失败：{error}"))?;
		let rectangles: &Vec<[i32; 4]> = floors.get(&self.config.ground_radius.to_string()).ok_or("缺少对应半径的地板轮廓。")?;
		// Resolve each random material once per round. Keep the saved settings intact.
		let mut round_config: Config = self.config.clone();
		if round_config.pillar_block == RANDOM_MATERIAL {
			let index: usize = self.random(PILLAR_MATERIALS.len());
			round_config.pillar_block = PILLAR_MATERIALS[index].to_string();
		}
		if round_config.ground_block == RANDOM_MATERIAL {
			let index: usize = self.random(GROUND_MATERIALS.len());
			round_config.ground_block = GROUND_MATERIALS[index].to_string();
		}
		self.round_config = round_config;
		self.test_mode = test_mode;
		self.members.clear();
		self.build.clear();
		self.nuked.clear();
		self.melting.clear();
		self.shrinks = 0;
		self.supply = 0;
		self.magnet_target = None;
		self.event = EventKind::Nothing;
		self.pending_event = None;
		self.event_ends = None;
		for &[x1, z1, x2, z2] in rectangles {
			self.build.push_back(Action::Command(format!("fill {x1} {} {z1} {x2} 0 {z2} {} strict", 1 - self.config.ground_layers, self.round_config.ground_block)));
		}
		if self.config.fishing {
			let radius: i32 = self.config.ground_radius;
			self.build.push_back(Action::Command(format!("fill -{radius} 1 -{radius} {radius} 1 {radius} water strict")));
		}
		let count: usize = if self.config.teams > 0 { usize::from(self.config.teams) } else { selected.len() };
		let mut order: Vec<usize> = (0..selected.len()).collect();
		for index in (1..order.len()).rev() {
			let other: usize = self.random(index + 1);
			order.swap(index, other);
		}
		let mut spawns: Vec<Position> = Vec::new();
		for index in 0..count {
			let angle: f64 = std::f64::consts::TAU * index as f64 / count as f64;
			let x: i32 = (angle.cos() * f64::from(self.config.pillar_radius)).round() as i32;
			let z: i32 = (angle.sin() * f64::from(self.config.pillar_radius)).round() as i32;
			let height: i32 = self.config.pillar_height;
			self.build.push_back(Action::Command(format!("fill {x} 1 {z} {x} {height} {z} {} strict", self.round_config.pillar_block)));
			self.build.push_back(Action::Command(format!("fill {} {height} {} {} {height} {} {} strict", x - 1, z - 1, x + 1, z + 1, self.round_config.pillar_block)));
			spawns.push((f64::from(x) + 0.5, f64::from(height) + 1.0, f64::from(z) + 0.5));
		}
		self.build.push_back(Action::Command("fill -24 129 -24 24 129 24 barrier strict".to_string()));
		for (index, selection) in order.iter().enumerate() {
			let player: &Snapshot = &selected[*selection];
			let team: usize = index % count;
			self.members.insert(player.id.clone(), Member {
				name: player.name.clone(), team, spawn: spawns[team], alive: true, spectated: false,
				star_until: 0, creative_until: 0, rescue_until: 0, gravity: false, gravity_until: 0, cooldown: 0,
			});
		}
		self.phase = Phase::Building;
		self.loading_ticks = 0;
		let mut actions: Vec<Action> = vec![Action::Announce(format!("正在加载 {} 人的比赛场地……", selected.len()))];
		for id in self.members.keys() { self.seen.insert(id.clone()); actions.push(Action::Player(id.clone(), Power::Load)); }
		// Run only after the players' view tickets have loaded the complete arena.
		self.build.push_front(Action::Command("kill @e[type=!player,x=-24,y=-64,z=-24,dx=48,dy=383,dz=48]".to_string()));
		Ok(actions)
	}

	pub fn needs_cleaning(&self) -> bool {
		self.active && matches!(self.phase, Phase::Lobby | Phase::Building) && self.clean_y.is_some() && !self.clean_busy
	}

	pub fn begin_cleaning(&mut self) -> Option<(u64, i32)> {
		if !self.needs_cleaning() { return None; }
		self.clean_busy = true;
		self.clean_y.map(|y: i32| (self.clean_epoch, y))
	}

	pub fn finish_cleaning(&mut self, epoch: u64, y: i32) {
		if self.clean_epoch != epoch { return; }
		self.clean_y = if y - 2 >= -64 { Some(y - 2) } else { None };
		self.clean_busy = false;
	}

	pub fn checking_ground(&self) -> bool {
		self.phase == Phase::Building && self.build.is_empty() && self.clean_y.is_none() && !self.clean_busy
	}

	pub fn arena_config(&self) -> &Config { &self.round_config }

	pub fn tick(&mut self, players: &[Snapshot], arena_loaded: bool) -> Vec<Action> {
		if !self.active { return Vec::new(); }
		self.clock += 5;
		let mut actions: Vec<Action> = Vec::new();
		let online: HashSet<String> = players.iter().map(|player: &Snapshot| player.id.clone()).collect();
		self.manual_ready.retain(|id: &String| online.contains(id));
		self.seen.retain(|id: &String| online.contains(id));
		for player in players {
			if self.seen.insert(player.id.clone()) {
				let power: Power = if matches!(self.phase, Phase::Building | Phase::Running | Phase::Ending) { Power::Spectate } else { Power::Lobby };
				actions.push(Action::Player(player.id.clone(), power));
			}
		}
		if self.phase == Phase::Lobby { return actions; }
		if self.phase == Phase::Building {
			if self.members.iter().any(|(id, member): (&String, &Member)| !online.contains(id) || !member.alive) {
				actions.extend(self.reset(players));
				actions.push(Action::Announce("场地准备期间有人离开，本局已取消。".to_string()));
				return actions;
			}
			if !arena_loaded {
				self.loading_ticks += 5;
				if self.loading_ticks >= 600 {
					actions.extend(self.reset(players));
					actions.push(Action::Announce("比赛区块加载超时，本局已取消。请检查客户端及服务端视距，场地未进行修改。".to_string()));
				}
				return actions;
			}
			self.loading_ticks = 0;
			if self.clean_y.is_some() || self.clean_busy { return actions; }
			if !self.build.is_empty() {
				for _ in 0..4 {
					let Some(action) = self.build.pop_front() else { break; };
					actions.push(action);
				}
				return actions;
			}
			self.phase = Phase::Running;
			self.remaining = 60;
			self.round_time = 0;
			self.item_time = u64::from(self.config.item_interval) * 20;
			self.event_time = u64::from(self.config.event_interval) * 20;
			actions.extend([Action::Border(49.0), Action::Rules(true, self.config.natural_regeneration)]);
			for (id, member) in &self.members {
				actions.push(Action::Player(id.clone(), Power::Start { pos: member.spawn, health: if self.config.double_health { 40.0 } else { 20.0 }, team: member.team, cage: self.config.spawn_protection }));
				if self.config.night_vision { actions.push(Action::Player(id.clone(), Power::Cast("night_vision".to_string()))); }
				if self.config.initial_tool { actions.push(Action::Player(id.clone(), Power::Prop("pickaxe".to_string()))); }
				if self.config.fishing { actions.push(Action::Player(id.clone(), Power::Give("minecraft:fishing_rod".to_string(), 1))); }
			}
			actions.push(Action::Announce("已就位，3 秒后解除保护。".to_string()));
			return actions;
		}
		if self.phase == Phase::Ending {
			self.remaining = self.remaining.saturating_sub(5);
			if self.remaining == 0 { actions.extend(self.reset(players)); }
			return actions;
		}
		for (id, member) in &mut self.members {
			if !online.contains(id) { member.alive = false; }
		}
		if self.remaining > 0 {
			self.remaining = self.remaining.saturating_sub(5);
			if self.remaining == 0 {
				for (id, member) in &self.members {
					if member.alive { actions.push(Action::Player(id.clone(), Power::Unfreeze)); }
				}
				actions.push(Action::Announce("游戏开始！事件：无事发生。".to_string()));
				if self.config.bonus_prop { actions.extend(self.give_props()); }
			}
			return actions;
		}
		self.round_time += 5;
		for player in players {
			if let Some(member) = self.members.get_mut(&player.id) {
				if member.alive && player.health <= 0.0 { member.alive = false; }
				if member.alive && player.pos.1 < -8.0 {
					if member.star_until > self.clock {
						member.rescue_until = self.clock + 40;
						actions.push(Action::Player(player.id.clone(), Power::Teleport(member.spawn)));
					} else if let Some(slot) = player.charm_slot {
						member.rescue_until = self.clock + 40;
						actions.push(Action::Player(player.id.clone(), Power::Rescue(slot, member.spawn)));
					}
					else { member.alive = false; }
				}
				if !member.alive && !member.spectated {
					member.spectated = true;
					actions.push(Action::Player(player.id.clone(), Power::Spectate));
					actions.push(Action::Announce(format!("{} 已被淘汰。", member.name)));
				}
				if member.star_until > 0 && member.star_until <= self.clock {
					member.star_until = 0;
					actions.push(Action::Player(player.id.clone(), Power::Star(false)));
				}
				if member.creative_until > 0 && member.creative_until <= self.clock {
					member.creative_until = 0;
					actions.push(Action::Player(player.id.clone(), Power::Creative(false)));
				}
				if member.gravity && member.gravity_until <= self.clock {
					member.gravity = false;
					member.gravity_until = 0;
					actions.push(Action::Player(player.id.clone(), Power::Gravity(false)));
					actions.push(Action::Player(player.id.clone(), Power::Ui("漂浮效果已结束。".to_string())));
				}
				if member.alive && member.gravity {
					actions.push(Action::Player(player.id.clone(), Power::PropProgress { prop: "gravity", remaining: member.gravity_until.saturating_sub(self.clock) }));
				}
				if member.alive && member.star_until > self.clock {
					actions.push(Action::Player(player.id.clone(), Power::PropProgress { prop: "star", remaining: member.star_until - self.clock }));
				}
				if member.alive && member.creative_until > self.clock {
					actions.push(Action::Player(player.id.clone(), Power::PropProgress { prop: "creative", remaining: member.creative_until - self.clock }));
				}
			} else if !player.spectator && player.health > 0.0 {
				actions.push(Action::Player(player.id.clone(), Power::Spectate));
			}
		}
		if self.event_ends.is_some_and(|ends: u64| self.clock >= ends) { actions.extend(self.end_event()); }
		if let Some((event, starts)) = self.pending_event {
			if self.clock >= starts {
				self.pending_event = None;
				actions.extend(self.begin_event(players, event));
			}
		}
		let factions: BTreeSet<usize> = self.members.values().filter(|member: &&Member| member.alive).map(|member: &Member| member.team).collect();
		if !self.test_mode && factions.len() <= 1 {
			let winners: Vec<String> = self.members.values().filter(|member: &&Member| member.alive).map(|member: &Member| member.name.clone()).collect();
			self.phase = Phase::Ending;
			actions.extend(self.end_event());
			self.remaining = 100;
			actions.push(Action::Announce(if winners.is_empty() { "本局平局，5 秒后返回大厅。".to_string() } else { format!("获胜：{}！5 秒后返回大厅。", winners.join("、")) }));
			for (id, member) in &mut self.members {
				if member.star_until > 0 {
					member.star_until = 0;
					actions.push(Action::Player(id.clone(), Power::Star(false)));
				}
				if member.creative_until > 0 {
					member.creative_until = 0;
					actions.push(Action::Player(id.clone(), Power::Creative(false)));
				}
				if member.gravity {
					member.gravity = false;
					member.gravity_until = 0;
					actions.push(Action::Player(id.clone(), Power::Gravity(false)));
				}
				if member.alive { actions.push(Action::Player(id.clone(), Power::Invulnerable(true))); }
			}
			return actions;
		}
		self.item_time = self.item_time.saturating_sub(5);
		if self.item_time == 0 {
			self.item_time = u64::from(self.config.item_interval) * 20;
			if !self.config.fishing { actions.extend(self.give_items(None)); }
		}
		self.event_time = self.event_time.saturating_sub(5);
		if self.event_time == 0 { actions.extend(self.next_event(players, None)); }
		actions.extend(self.event_tick(players));
		if self.round_time.is_multiple_of(20) {
			let alive: usize = self.members.values().filter(|member: &&Member| member.alive).count();
			for player in players {
				let text: String = format!("存活 {alive} | 发物 {}s | {} | 下次事件 {}s", self.item_time / 20, self.event_label(), self.event_time.div_ceil(20));
				actions.push(Action::Player(player.id.clone(), Power::Ui(text)));
			}
		}
		actions
	}

	pub fn give_items(&mut self, target: Option<&str>) -> Vec<Action> {
		let ids: Vec<String> = self.members.iter().filter(|(id, member): &(&String, &Member)| member.alive && target.is_none_or(|wanted: &str| wanted == id.as_str())).map(|(id, _): (&String, &Member)| id.clone()).collect();
		let same_prop: bool = self.random(100) < usize::from(if self.supply > 0 { 60 } else { self.config.prop_chance });
		let same_item: usize = self.random(self.items.len());
		let same_special: usize = self.random(PROPS.len());
		let mut actions: Vec<Action> = Vec::new();
		for id in ids {
			let prop: bool = if self.config.same_items { same_prop } else { self.random(100) < usize::from(if self.supply > 0 { 60 } else { self.config.prop_chance }) };
			if prop {
				let index: usize = if self.config.same_items { same_special } else { self.random(PROPS.len()) };
				actions.push(Action::Player(id, Power::Prop(PROPS[index].to_string())));
			} else {
				let index: usize = if self.config.same_items { same_item } else { self.random(self.items.len()) };
				actions.push(Action::Player(id, Power::Give(self.items[index].clone(), self.config.item_count)));
			}
		}
		if self.supply > 0 { self.supply -= 1; }
		actions
	}

	pub fn give_props(&mut self) -> Vec<Action> {
		let ids: Vec<String> = self.members.iter().filter(|(_, member): &(&String, &Member)| member.alive).map(|(id, _): (&String, &Member)| id.clone()).collect();
		ids.into_iter().map(|id: String| {
			let index: usize = self.random(PROPS.len());
			Action::Player(id, Power::Prop(PROPS[index].to_string()))
		}).collect()
	}

	pub fn use_prop(&mut self, id: &str, prop: &str) -> Option<Vec<Action>> {
		if !self.is_alive(id) || self.remaining > 0 || !PROPS.contains(&prop) || ["void_charm", "knockback", "pickaxe"].contains(&prop) { return None; }
		let member: &mut Member = self.members.get_mut(id)?;
		if member.cooldown > self.clock { return None; }
		member.cooldown = self.clock + 10;
		let power: Power = match prop {
			"star" => { member.star_until = self.clock + STAR_DURATION_TICKS; Power::Star(true) }
			"creative" => { member.creative_until = self.clock + CREATIVE_DURATION_TICKS; Power::Creative(true) }
			"gravity" => {
				member.gravity = true;
				member.gravity_until = self.clock + 200;
				Power::Gravity(true)
			}
			_ => Power::Cast(prop.to_string()),
		};
		Some(vec![Action::Player(id.to_string(), power)])
	}

	pub fn next_event(&mut self, players: &[Snapshot], forced: Option<EventKind>) -> Vec<Action> {
		if self.phase != Phase::Running || self.remaining > 0 { return Vec::new(); }
		let event: EventKind = if let Some(event) = forced { event } else {
			let index: usize = self.random(self.config.events.len());
			self.config.events[index]
		};
		let mut actions: Vec<Action> = self.end_event();
		self.pending_event = Some((event, self.clock + EVENT_WARNING_TICKS));
		self.event_time = u64::from(self.config.event_interval) * 20;
		self.melting.clear();
		for player in players { actions.push(Action::Player(player.id.clone(), Power::EventTitle(event))); }
		actions
	}

	fn end_event(&mut self) -> Vec<Action> {
		let active: bool = self.event_ends.take().is_some();
		self.pending_event = None;
		self.event = EventKind::Nothing;
		self.magnet_target = None;
		self.supply = 0;
		self.melting.clear();
		if !active { return Vec::new(); }
		self.build.clear();
		let mut actions: Vec<Action> = vec![Action::EndEvent];
		for (id, member) in &self.members {
			if member.alive {
				actions.push(Action::Player(id.clone(), Power::Event(EventKind::Nothing)));
			}
		}
		actions
	}

	fn begin_event(&mut self, players: &[Snapshot], event: EventKind) -> Vec<Action> {
		self.event = event;
		self.event_ends = Some(self.clock + EVENT_DURATION_TICKS);
		let mut actions: Vec<Action> = Vec::new();
		let alive: Vec<Snapshot> = players.iter().filter(|player: &&Snapshot| self.is_alive(&player.id)).cloned().collect();
		match event {
			EventKind::Border if self.shrinks < 3 => {
				self.shrinks += 1;
				let diameter: f64 = 49.0 - f64::from(self.shrinks) * 12.0;
				actions.push(Action::Border(diameter));
				actions.push(Action::Announce(format!("安全区将在 5 秒内收缩至 {diameter} 格宽，请向中心移动。")));
			}
			EventKind::Thunder if !alive.is_empty() => {
				let index: usize = self.random(alive.len());
				actions.push(Action::Player(alive[index].id.clone(), Power::Event(event)));
			}
			EventKind::Swap if alive.len() > 1 => {
				let offset: usize = self.random(alive.len() - 1) + 1;
				for (index, player) in alive.iter().enumerate() { actions.push(Action::Player(player.id.clone(), Power::Teleport(alive[(index + offset) % alive.len()].pos))); }
			}
			EventKind::Supply => { self.supply = 3; }
			EventKind::Magnet if !alive.is_empty() => {
				let index: usize = self.random(alive.len());
				self.magnet_target = Some(alive[index].id.clone());
				actions.push(Action::Player(alive[index].id.clone(), Power::Ui("吸星大法：你获得了 10 秒掉落物吸取能力。".to_string())));
			}
			EventKind::Harvest => {
				for player in &alive {
					if player.health < player.max_health { actions.push(Action::Player(player.id.clone(), Power::Heal)); }
					else { let index: usize = self.random(PROPS.len()); actions.push(Action::Player(player.id.clone(), Power::Prop(PROPS[index].to_string()))); }
				}
			}
			EventKind::Purge => actions.push(Action::Command("kill @e[type=!player,x=-24,y=-64,z=-24,dx=48,dy=383,dz=48]".to_string())),
			EventKind::Nuke if self.nuked.len() < 4 => {
				let mut quadrant: usize = self.random(4);
				while self.nuked.contains(&quadrant) { quadrant = (quadrant + 1) % 4; }
				self.nuked.insert(quadrant);
				let (x1, x2): (i32, i32) = if quadrant & 1 == 0 { (-24, -1) } else { (0, 24) };
				let (z1, z2): (i32, i32) = if quadrant & 2 == 0 { (-24, -1) } else { (0, 24) };
				for y in (-64..129).step_by(32) { self.build.push_back(Action::Command(format!("fill {x1} {y} {z1} {x2} {} {z2} air strict", (y + 31).min(128)))); }
			}
			EventKind::Moon | EventKind::Invisible | EventKind::Small => {
				for player in &alive { actions.push(Action::Player(player.id.clone(), Power::Event(event))); }
			}
			_ => {}
		}
		actions
	}

	fn event_tick(&mut self, players: &[Snapshot]) -> Vec<Action> {
		let mut actions: Vec<Action> = Vec::new();
		if self.event_ends.is_none() { return actions; }
		if let Some(ends) = self.event_ends {
			actions.push(Action::EventProgress { name: self.event.name().to_string(), remaining: ends.saturating_sub(self.clock), total: EVENT_DURATION_TICKS });
		}
		if let Some(action) = self.build.pop_front() { actions.push(action); }
		let alive: Vec<&Snapshot> = players.iter().filter(|player: &&Snapshot| self.is_alive(&player.id)).collect();
		if self.event == EventKind::Magnet {
			let targets: Vec<String> = alive.iter().filter(|player: &&&Snapshot| self.magnet_target.as_deref() == Some(player.id.as_str())).map(|player: &&Snapshot| player.id.clone()).collect();
			if !targets.is_empty() { actions.push(Action::Magnet(targets)); }
		}
		for player in alive {
			if !(-24.0..25.0).contains(&player.pos.0) || !(-24.0..25.0).contains(&player.pos.2) || !(-63.0..129.0).contains(&player.pos.1) { continue; }
			match self.event {
				EventKind::Arrows | EventKind::Iron if self.round_time.is_multiple_of(20) => actions.push(Action::Player(player.id.clone(), Power::Event(self.event))),
				EventKind::Lava if player.has_event_floor() => {
					let pos: (i32, i32, i32) = floor_position(player.pos);
					self.melting.entry(pos).or_insert(MeltingBlock { started: self.clock, original: player.below.trim_start_matches("minecraft:").to_string() });
				}
				EventKind::Touch if player.has_event_floor() => {
					const BLOCKS: [&str; 7] = ["stone", "slime_block", "glass", "ice", "magma_block", "honey_block", "oak_planks"];
					let index: usize = self.random(BLOCKS.len());
					let (x, y, z): (i32, i32, i32) = floor_position(player.pos);
					let original: &str = player.below.trim_start_matches("minecraft:");
					actions.push(Action::Command(format!("fill {x} {y} {z} {x} {y} {z} {} replace {original}", BLOCKS[index])));
				}
				_ => {}
			}
		}
		if self.event == EventKind::Lava {
			for ((x, y, z), melting) in &self.melting {
				let age: u64 = self.clock - melting.started;
				if age.is_multiple_of(20) && age <= 60 {
					const STAGES: [&str; 4] = ["yellow_wool", "orange_wool", "red_wool", "lava"];
					let stage: usize = (age / 20) as usize;
					let original: &str = if stage == 0 { &melting.original } else { STAGES[stage - 1] };
					let block: &str = STAGES[stage];
					actions.push(Action::Command(format!("fill {x} {y} {z} {x} {y} {z} {block} replace {original}")));
				}
			}
		}
		actions
	}
}
