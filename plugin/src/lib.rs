mod config;
mod effects;
mod game;
mod lobby;
mod runtime;
mod transfer;

use config::{Config, EventKind, RANDOM_MATERIAL};
use game::{Action, Game, Phase, Power, Snapshot, PROPS};
use pumpkin_plugin_api::{
	BlockType, Context, DamageType, Item, Player, Plugin, PluginMetadata, Server,
	command::{Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType},
	commands::CommandHandler,
	events::{EntityDamageEvent, EventData, EventHandler, EventPriority, InteractAction, PlayerChangedWorldEvent, PlayerDeathEvent, PlayerFishEvent, PlayerInteractEvent, PlayerInteractEntityEvent, PlayerLeaveEvent, PlayerMoveEvent, PlayerRespawnEvent, PlayerSpawnLocationEvent, PlayerTeleportEvent},
	common::Hand,
	permission::{Permission, PermissionDefault, PermissionLevel},
	permissions::FS_WRITE_DATA,
	scheduler::SchedulerExt,
	text::TextComponent,
	wit::pumpkin::plugin::{event::PlayerFishState, uuid},
	register_plugin,
};
use std::{collections::HashSet, fs, path::PathBuf, sync::{Arc, Mutex, MutexGuard}};

const PLAY_PERMISSION: &str = "pillars-of-fortune:play";
const ADMIN_PERMISSION: &str = "pillars-of-fortune:admin";
const SETTINGS_PERMISSION: &str = if cfg!(feature = "admin-settings") { ADMIN_PERMISSION } else { PLAY_PERMISSION };

struct App {
	game: Mutex<Game>,
	config_path: Mutex<PathBuf>,
	startup: Mutex<Startup>,
	startup_players: Mutex<HashSet<String>>,
	lobby: Mutex<Option<lobby::Lobby>>,
}

#[derive(Clone)]
enum Startup { Waiting(u32), Ready, Failed(String) }

impl App {
	fn game(&self) -> MutexGuard<'_, Game> { self.game.lock().unwrap_or_else(|error| error.into_inner()) }

	fn startup(&self) -> MutexGuard<'_, Startup> { self.startup.lock().unwrap_or_else(|error| error.into_inner()) }

	fn activate_lobby_button(&self, server: &Server, player: &Player, command: &str) -> Result<(), String> {
		if !matches!(*self.startup(), Startup::Ready) { return Err("大厅区块仍在加载，请稍候。".to_string()); }
		let public: bool = matches!(command, "join" | "leave" | "status" | "start");
		let permission: &str = if command.starts_with("set ") { SETTINGS_PERMISSION } else if public { PLAY_PERMISSION } else { ADMIN_PERMISSION };
		if !player.has_permission(permission) {
			return Err(if permission == ADMIN_PERMISSION { "此按钮需要管理员权限，请联系服主。" } else { "你没有使用此按钮的权限。" }.to_string());
		}
		if command == "join" {
			let id: String = player.get_id().to_string();
			self.game().ready(&id, true)?;
			runtime::message(player, "已准备参赛，可点击取消准备面板退出。", true);
		} else if let Some(sender) = server.get_player_by_uuid(player.get_id()) {
			server.execute_command(&format!("pof {command}"), pumpkin_plugin_api::server::CommandSender::Player(sender));
		}
		Ok(())
	}

	fn refresh_lobby(&self, server: &Server, players: usize, force: bool) {
		let panel: Option<lobby::Lobby> = self.lobby.lock().unwrap_or_else(|error| error.into_inner()).take();
		if let Some(mut panel) = panel {
			let (config, phase, ready): (Config, Phase, usize) = { let game: MutexGuard<'_, Game> = self.game(); (game.config.clone(), game.phase, game.ready_count()) };
			panel.tick(server, &config, phase, players, ready, force);
			*self.lobby.lock().unwrap_or_else(|error| error.into_inner()) = Some(panel);
		}
	}

	fn bootstrap(&self, server: &Server) -> bool {
		let state: Startup = self.startup().clone();
		let attempts: u32 = match state {
			Startup::Ready => return true,
			Startup::Failed(_) => return false,
			Startup::Waiting(attempts) => attempts,
		};
		let clients: Vec<Player> = runtime::world_players(server);
		let ids: HashSet<String> = clients.iter().map(|player: &Player| player.get_id().to_string()).collect();
		self.startup_players.lock().unwrap_or_else(|error| error.into_inner()).retain(|id: &String| ids.contains(id));
		if clients.is_empty() { *self.startup() = Startup::Waiting(0); return false; }
		for player in &clients {
			let id: String = player.get_id().to_string();
			let joined: bool = self.startup_players.lock().unwrap_or_else(|error| error.into_inner()).insert(id);
			if joined {
				runtime::execute(server, vec![Action::Player(player.get_id().to_string(), Power::LobbyLoad)]);
				runtime::message(player, "大厅区块加载中，请稍候……", false);
			}
		}
		match runtime::poll_world(server) {
			Ok(true) => {
				runtime::prepare_lobby(server);
				let players: Vec<Snapshot> = runtime::snapshots(server);
				let actions: Vec<Action> = {
					let mut game: MutexGuard<'_, Game> = self.game();
					game.active = true;
					game.reset(&players)
				};
				*self.startup() = Startup::Ready;
				runtime::execute(server, actions);
				tracing::info!("幸运之柱大厅区块已就绪，玩法已启用；比赛区块将在开局时加载。");
				false
			}
			Ok(false) if attempts < 119 => {
				*self.startup() = Startup::Waiting(attempts + 1);
				false
			}
			result => {
				let reason: String = result.err().unwrap_or_else(|| "玩家入服后大厅区块加载超时（30 秒），请检查客户端及服务端视距和区块加载日志。".to_string());
				tracing::error!("幸运之柱玩法未启用：{reason}");
				*self.startup() = Startup::Failed(reason);
				runtime::unlock_players(server);
				false
			}
		}
	}
}

struct PillarsOfFortune { app: Arc<App> }

impl Plugin for PillarsOfFortune {
	fn new() -> Self {
		Self { app: Arc::new(App { game: Mutex::new(Game::new(Config::default(), vec!["minecraft:stone".to_string()], 1)), config_path: Mutex::new(PathBuf::new()), startup: Mutex::new(Startup::Waiting(0)), startup_players: Mutex::new(HashSet::new()), lobby: Mutex::new(Some(lobby::Lobby::default())) }) }
	}

	fn metadata(&self) -> PluginMetadata {
		PluginMetadata {
			name: "pillars-of-fortune".to_string(), version: env!("CARGO_PKG_VERSION").to_string(),
			authors: vec!["JK137 (original map)".to_string()],
			description: "幸运之柱 Pumpkin Rust 玩法移植".to_string(), dependencies: Vec::new(),
			permissions: vec![FS_WRITE_DATA.to_string()],
		}
	}

	fn on_load(&self, context: Context) -> Result<(), String> {
		let server: Server = context.get_server();
		transfer::clear();
		runtime::validate_world(&server)?;
		let data: PathBuf = PathBuf::from(context.get_data_folder());
		fs::create_dir_all(&data).map_err(|error: std::io::Error| error.to_string())?;
		let path: PathBuf = data.join("config.json");
		let config: Config = Config::load(&path)?;
		validate_blocks(&config)?;
		let items: Vec<String> = serde_json::from_str::<Vec<String>>(include_str!("../data/items.json"))
			.map_err(|error: serde_json::Error| error.to_string())?
			.into_iter().filter(|name: &String| Item::from_registry_key(name).is_some()).collect();
		if items.is_empty() { return Err("物品池为空。".to_string()); }
		let seed: u64 = uuid::generate().low;
		*self.app.game() = Game::new(config, items, seed);
		self.app.game().active = false;
		*self.app.startup() = Startup::Waiting(0);
		*self.app.config_path.lock().unwrap_or_else(|error| error.into_inner()) = path;
		context.register_permission(&Permission { node: PLAY_PERMISSION.to_string(), description: "准备、开始比赛和观战".to_string(), default: PermissionDefault::Allow, children: Vec::new() })?;
		context.register_permission(&Permission { node: ADMIN_PERMISSION.to_string(), description: "管理幸运之柱比赛".to_string(), default: PermissionDefault::Op(PermissionLevel::Two), children: Vec::new() })?;
		let command: Command = Command::new(&["pof".to_string()], "幸运之柱")
			.execute(Commands { app: Arc::clone(&self.app), arguments: false })
			.then(CommandNode::argument("arguments", &ArgumentType::String(StringType::Greedy)).execute(Commands { app: Arc::clone(&self.app), arguments: true }));
		context.register_command(command, PLAY_PERMISSION);
		context.register_event_handler::<PlayerDeathEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<EntityDamageEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Highest, true)?;
		context.register_event_handler::<PlayerLeaveEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerChangedWorldEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerRespawnEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerSpawnLocationEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerInteractEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerInteractEntityEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerFishEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerMoveEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		context.register_event_handler::<PlayerTeleportEvent, _>(Hooks(Arc::clone(&self.app)), EventPriority::Normal, true)?;
		runtime::command(&server, "gamerule spectators_generate_chunks true");
		if let Some(world) = runtime::world(&server) { effects::clear_iron(&world); }
		tracing::info!("幸运之柱已加载，等待玩家进入大厅以加载区块；空服时不会计时超时。");
		tracing::warn!("当前插件不支持 Bedrock（基岩版），请使用 Java 版客户端。");
		let app: Arc<App> = Arc::clone(&self.app);
		let task: u32 = context.schedule_repeating_task(5, 5, move |server: Server| {
			runtime::execute(&server, transfer::drain());
			if !app.bootstrap(&server) { return; }
			runtime::unlock_lobby(&server);
			let players: Vec<Snapshot> = runtime::snapshots(&server);
			let building: bool = app.game().phase == Phase::Building;
			let arena_loaded: bool = !building || runtime::arena_ready(&server);
			let floor_config: Option<Config> = { let game: MutexGuard<'_, Game> = app.game(); if arena_loaded && game.checking_ground() { Some(game.arena_config().clone()) } else { None } };
			let floor_error: Option<String> = floor_config.as_ref().and_then(|config: &Config| runtime::verify_ground(&server, config).err());
			let actions: Vec<Action> = if let Some(reason) = floor_error {
				tracing::error!("{reason}");
				let mut actions: Vec<Action> = app.game().reset(&players);
				actions.push(Action::Announce(format!("场地未通过校验，已取消本局。{reason}")));
				actions
			} else { app.game().tick(&players, arena_loaded) };
			// The state guard is dropped before any host calls (which can re-enter us).
			runtime::execute(&server, actions);
			let running: bool = app.game().phase == Phase::Running;
			if running { runtime::contain_entities(&server); }
			app.refresh_lobby(&server, players.len(), false);
		});
		self.app.game().task = Some(task);
		let app: Arc<App> = Arc::clone(&self.app);
		let clean_task: u32 = context.schedule_repeating_task(1, 1, move |server: Server| {
			effects::tick_border(&server);
			effects::tick_iron(&server);
			let pending: bool = app.game().needs_cleaning();
			if !pending || !runtime::arena_ready(&server) { return; }
			let step: Option<(u64, i32)> = app.game().begin_cleaning();
			let Some((epoch, y)) = step else { return; };
			// Match the datapack's descending two-layer sweep, clipped to world min Y.
			runtime::command(&server, &format!("fill -24 {} -24 24 {y} 24 air strict", (y - 1).max(-64)));
			app.game().finish_cleaning(epoch, y);
		});
		self.app.game().clean_task = Some(clean_task);
		Ok(())
	}

	fn on_unload(&self, _context: Context) -> Result<(), String> {
		// The host disables scheduled tasks before calling this export. Its store is
		// already shutting down: keep this callback free of world/player host calls.
		{
			let mut game: MutexGuard<'_, Game> = self.app.game();
			game.active = false;
			game.task = None;
			game.clean_task = None;
		}
		*self.app.lobby.lock().unwrap_or_else(|error| error.into_inner()) = None;
		self.app.startup_players.lock().unwrap_or_else(|error| error.into_inner()).clear();
		transfer::clear();
		Ok(())
	}
}

fn validate_blocks(config: &Config) -> Result<(), String> {
	for block in [&config.pillar_block, &config.ground_block] {
		if block == RANDOM_MATERIAL { continue; }
		if BlockType::from_registry_key(block).is_none() { return Err(format!("未知方块：{block}")); }
	}
	Ok(())
}

struct Commands { app: Arc<App>, arguments: bool }

impl CommandHandler for Commands {
	fn handle(&self, sender: CommandSender, server: Server, args: ConsumedArgs) -> Result<i32, CommandError> {
		let text: String = if self.arguments { match args.get_value("arguments") { Arg::Simple(value) | Arg::Msg(value) => value, _ => String::new() } } else { String::new() };
		let result: Result<Vec<Action>, String> = self.run(&sender, &server, &text);
		match result {
			Ok(actions) => { runtime::execute(&server, actions); Ok(1) }
			Err(error) => Err(CommandError::CommandFailed(TextComponent::text(&error))),
		}
	}
}

impl Commands {
	fn run(&self, sender: &CommandSender, server: &Server, input: &str) -> Result<Vec<Action>, String> {
		let words: Vec<&str> = input.split_whitespace().collect();
		let action: &str = words.first().copied().unwrap_or("help");
		if action == "help" {
			sender.send_message(TextComponent::text("请右键大厅准备面板参赛，点击开始游戏面板开局。/pof start 开局；leave 退出；status 状态。\n设置：set <配置名> <JSON值>、reload、config。\n管理：test、stop、event [事件ID]、give、props。"));
			return Ok(Vec::new());
		}
		let startup: Startup = self.app.startup().clone();
		match startup {
			Startup::Waiting(_) => return Err("等待玩家进入大厅并加载区块，请入服后稍候再试。".to_string()),
			Startup::Failed(reason) => return Err(format!("玩法未启用：{reason}")),
			Startup::Ready => {}
		}
		if action == "status" { let status: String = self.app.game().status(); sender.send_message(TextComponent::text(&status)); return Ok(Vec::new()); }
		if action == "join" { return Err("请右键大厅的准备面板确认参赛。".to_string()); }
		if action == "leave" {
			let player: Player = sender.as_player().ok_or("此命令需要由玩家执行。")?;
			let arena: pumpkin_plugin_api::World = runtime::world(server).ok_or("主世界未加载。")?;
			if player.get_world().get_id() != arena.get_id() { return Err("请先进入幸运之柱主世界。".to_string()); }
			let id: String = player.get_id().to_string();
			let mut actions: Vec<Action> = Vec::new();
			{
				let mut game: MutexGuard<'_, Game> = self.app.game();
				if matches!(game.phase, Phase::Building | Phase::Running) { game.mark_dead(&id); actions.push(Action::Player(id.clone(), Power::Spectate)); }
				else { game.ready(&id, false)?; }
			}
			runtime::message(&player, "已退出准备或比赛。", true);
			return Ok(actions);
		}
		if action == "start" {
			if let Some(player) = sender.as_player() {
				let arena: pumpkin_plugin_api::World = runtime::world(server).ok_or("主世界未加载。")?;
				if player.get_world().get_id() != arena.get_id() { return Err("请先进入幸运之柱主世界。".to_string()); }
			}
			let players: Vec<Snapshot> = runtime::snapshots(server);
			return self.app.game().start(&players, false);
		}
		let permission: &str = if matches!(action, "config" | "set" | "reload") { SETTINGS_PERMISSION } else { ADMIN_PERMISSION };
		if !sender.has_permission(server, permission) { return Err(format!("需要 {permission} 权限。")); }
		if action == "config" {
			let config: Config = self.app.game().config.clone();
			sender.send_message(TextComponent::text(&serde_json::to_string_pretty(&config).map_err(|error: serde_json::Error| error.to_string())?));
			return Ok(Vec::new());
		}
		if matches!(action, "set" | "reload") {
			if self.app.game().phase != Phase::Lobby { return Err("请先 /pof stop，回到大厅后修改设置。".to_string()); }
			let path: PathBuf = self.app.config_path.lock().unwrap_or_else(|error| error.into_inner()).clone();
			let config: Config = if action == "reload" { Config::load(&path)? } else {
				let key: &str = words.get(1).ok_or("用法：/pof set item_interval 5")?;
				let value: String = words.iter().skip(2).copied().collect::<Vec<&str>>().join(" ");
				if value.is_empty() { return Err("缺少配置值。".to_string()); }
				let current: Config = self.app.game().config.clone();
				let mut json: serde_json::Value = serde_json::to_value(current).map_err(|error: serde_json::Error| error.to_string())?;
				if json.get(key).is_none() { return Err(format!("未知配置：{key}")); }
				json[key] = serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value));
				serde_json::from_value(json).map_err(|error: serde_json::Error| error.to_string())?
			};
			config.validate()?; validate_blocks(&config)?; config.save(&path)?;
			self.app.game().config = config;
			self.app.refresh_lobby(server, runtime::world_players(server).len(), true);
			if let Some(player) = sender.as_player() {
				runtime::message(&player, "设置已保存，将在下局使用。", true);
			} else {
				sender.send_message(TextComponent::text("设置已保存，将在下局使用。"));
			}
			return Ok(Vec::new());
		}
		let players: Vec<Snapshot> = runtime::snapshots(server);
		let mut game: MutexGuard<'_, Game> = self.app.game();
		match action {
			"test" => game.start(&players, true),
			"stop" => Ok(game.reset(&players)),
			"event" => {
				let event: Option<EventKind> = words.get(1).map(|name: &&str| serde_json::from_value(serde_json::Value::String((*name).to_string()))).transpose().map_err(|error: serde_json::Error| error.to_string())?;
				if game.phase != Phase::Running { return Err("当前没有进行中的比赛。".to_string()); }
				Ok(game.next_event(&players, event))
			}
			"give" => Ok(game.give_items(None)),
			"props" => {
				let ids: Vec<String> = game.members.iter().filter(|(_, member): &(&String, &game::Member)| member.alive).map(|(id, _): (&String, &game::Member)| id.clone()).collect();
				Ok(ids.iter().flat_map(|id: &String| PROPS.iter().map(move |prop: &&str| Action::Player(id.clone(), Power::Prop((*prop).to_string())))).collect())
			}
			_ => Err("未知子命令，请使用 /pof 查看帮助。".to_string()),
		}
	}
}

struct Hooks(Arc<App>);

impl EventHandler<EntityDamageEvent> for Hooks {
	fn handle(&self, server: Server, mut event: EventData<EntityDamageEvent>) -> EventData<EntityDamageEvent> {
		if event.cancelled { return event; }
		let player: Option<Player> = server.get_all_players().into_iter().find(|player: &Player| i64::from(player.as_entity().get_id()) == i64::from(event.entity_id));
		if let Some(player) = player {
			let id: String = player.get_id().to_string();
			let game: MutexGuard<'_, Game> = self.0.game();
			if game.has_star(&id) || (event.damage_type == DamageType::Fall && game.has_rescue_protection(&id)) { event.cancelled = true; }
		}
		event
	}
}

impl EventHandler<PlayerInteractEntityEvent> for Hooks {
	fn handle(&self, server: Server, mut event: EventData<PlayerInteractEntityEvent>) -> EventData<PlayerInteractEntityEvent> {
		if event.cancelled { return event; }
		let Ok(entity_id) = u32::try_from(event.entity_id) else { return event; };
		let id: String = event.player.get_id().to_string();
		let position: game::Position = event.player.get_position();
		let Some(world) = runtime::world(&server) else { return event; };
		if event.player.get_world().get_id() != world.get_id() { return event; }
		let attack: bool = matches!(event.action, pumpkin_plugin_api::events_wit::EntityInteractionAction::Attack);
		let command: Option<String> = {
			let mut panel: MutexGuard<'_, Option<lobby::Lobby>> = self.0.lobby.lock().unwrap_or_else(|error| error.into_inner());
			let Some(panel) = panel.as_mut() else { return event; };
			if !panel.owns(entity_id) { return event; }
			event.cancelled = true;
			if attack { None } else { panel.click(entity_id, &id, position) }
		};
		if let Some(command) = command {
			if let Err(error) = self.0.activate_lobby_button(&server, &event.player, &command) { runtime::message(&event.player, &error, true); }
		}
		event
	}
}

impl EventHandler<PlayerDeathEvent> for Hooks {
	fn handle(&self, _server: Server, event: EventData<PlayerDeathEvent>) -> EventData<PlayerDeathEvent> {
		let id: String = event.player.get_id().to_string();
		if !event.cancelled {
			self.0.game().mark_dead(&id);
			effects::clear_prop_bars(&id);
		}
		event
	}
}

impl EventHandler<PlayerLeaveEvent> for Hooks {
	fn handle(&self, _server: Server, event: EventData<PlayerLeaveEvent>) -> EventData<PlayerLeaveEvent> {
		let id: String = event.player.get_id().to_string();
		if !event.cancelled {
			transfer::forget(&id);
			let participant: bool = self.0.game().members.contains_key(&id);
			self.0.game().mark_dead(&id);
			if participant { runtime::release_player(&event.player); }
		}
		event
	}
}

impl EventHandler<PlayerChangedWorldEvent> for Hooks {
	fn handle(&self, server: Server, event: EventData<PlayerChangedWorldEvent>) -> EventData<PlayerChangedWorldEvent> {
		let id: String = event.player.get_id().to_string();
		let departed: bool = runtime::world(&server).is_some_and(|arena: pumpkin_plugin_api::World| event.from_world.get_id() == arena.get_id() && event.to_world.get_id() != arena.get_id());
		if departed && !event.cancelled { transfer::forget(&id); self.0.game().mark_dead(&id); runtime::release_player(&event.player); }
		event
	}
}

impl EventHandler<PlayerMoveEvent> for Hooks {
	fn handle(&self, _server: Server, event: EventData<PlayerMoveEvent>) -> EventData<PlayerMoveEvent> {
		if !event.cancelled {
			let id: String = event.player.get_id().to_string();
			transfer::moved(&id);
		}
		event
	}
}

impl EventHandler<PlayerTeleportEvent> for Hooks {
	fn handle(&self, _server: Server, event: EventData<PlayerTeleportEvent>) -> EventData<PlayerTeleportEvent> {
		if !event.cancelled {
			let id: String = event.player.get_id().to_string();
			transfer::requested(&id);
		}
		event
	}
}

impl EventHandler<PlayerRespawnEvent> for Hooks {
	fn handle(&self, _server: Server, event: EventData<PlayerRespawnEvent>) -> EventData<PlayerRespawnEvent> {
		let id: String = event.player.get_id().to_string();
		let participant: bool = self.0.game().members.contains_key(&id);
		if participant { self.0.game().mark_dead(&id); }
		event
	}
}

impl EventHandler<PlayerSpawnLocationEvent> for Hooks {
	fn handle(&self, server: Server, mut event: EventData<PlayerSpawnLocationEvent>) -> EventData<PlayerSpawnLocationEvent> {
		if event.cancelled { return event; }
		let id: String = event.player.get_id().to_string();
		let participant: bool = self.0.game().members.contains_key(&id);
		if participant && runtime::world(&server).is_some_and(|world: pumpkin_plugin_api::World| world.get_id() == event.player.get_world().get_id()) {
			event.spawn_pos = (0.5, 120.0, 0.5);
		}
		event
	}
}

impl EventHandler<PlayerInteractEvent> for Hooks {
	fn handle(&self, server: Server, mut event: EventData<PlayerInteractEvent>) -> EventData<PlayerInteractEvent> {
		if event.cancelled || !matches!(event.action, InteractAction::RightClickAir | InteractAction::RightClickBlock) { return event; }
		let Some(stack) = event.player.get_item_in_hand(Hand::Right) else { return event; };
		let Some(prop) = runtime::prop_id(&stack) else { return event; };
		let pos: game::Position = event.player.get_position();
		if !(-24.0..25.0).contains(&pos.0) || !(-24.0..25.0).contains(&pos.2) || !(-64.0..129.0).contains(&pos.1) { return event; }
		let id: String = event.player.get_id().to_string();
		let actions: Option<Vec<Action>> = self.0.game().use_prop(&id, &prop);
		if let Some(actions) = actions {
			event.cancelled = true;
			let count: u8 = stack.get_count();
			if count <= 1 { event.player.set_item_in_hand(Hand::Right, None); }
			else { stack.set_count(count - 1); event.player.set_item_in_hand(Hand::Right, Some(stack)); }
			runtime::execute(&server, actions);
		}
		event
	}
}

impl EventHandler<PlayerFishEvent> for Hooks {
	fn handle(&self, server: Server, mut event: EventData<PlayerFishEvent>) -> EventData<PlayerFishEvent> {
		if event.cancelled || event.state != PlayerFishState::CaughtFish { return event; }
		let id: String = event.player.get_id().to_string();
		let fishing: bool = { let game: MutexGuard<'_, Game> = self.0.game(); game.config.fishing && game.is_alive(&id) };
		if fishing {
			event.cancelled = true;
			let actions: Vec<Action> = self.0.game().give_items(Some(&id));
			runtime::execute(&server, actions);
		}
		event
	}
}

register_plugin!(PillarsOfFortune);
