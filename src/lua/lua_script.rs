use super::constants::*;
use super::user_data::*;
use super::{util::*, UiEvent};
use crate::{event::Event, model::Line};
use anyhow::Result;
use rlua::{Lua, Result as LuaResult};
use std::io::prelude::*;
use std::{fs::File, sync::mpsc::Sender};

pub struct LuaScript {
    state: Lua,
    writer: Sender<Event>,
    on_connect_triggered: bool,
    on_gmcp_ready_triggered: bool,
}

fn create_default_lua_state(writer: Sender<Event>, dimensions: (u16, u16)) -> Lua {
    let state = Lua::new();

    let mut blight = BlightMud::new(writer);
    blight.screen_dimensions = dimensions;
    state
        .context(|ctx| -> LuaResult<()> {
            let globals = ctx.globals();
            globals.set("blight", blight)?;

            let alias_table = ctx.create_table()?;
            globals.set(ALIAS_TABLE, alias_table)?;
            let trigger_table = ctx.create_table()?;
            globals.set(TRIGGER_TABLE, trigger_table)?;
            let prompt_trigger = ctx.create_table()?;
            globals.set(PROMPT_TRIGGER_TABLE, prompt_trigger)?;
            let gmcp_listener_table = ctx.create_table()?;
            globals.set(GMCP_LISTENER_TABLE, gmcp_listener_table)?;
            let timed_func_table = ctx.create_table()?;
            globals.set(TIMED_FUNCTION_TABLE, timed_func_table)?;
            let command_bind_table = ctx.create_table()?;
            globals.set(COMMAND_BINDING_TABLE, command_bind_table)?;

            globals.set(GAG_NEXT_TRIGGER_LINE, false)?;

            let lua_json = ctx
                .load(include_str!("../../resources/lua/json.lua"))
                .call::<_, rlua::Value>(())?;
            globals.set("json", lua_json)?;

            ctx.load(include_str!("../../resources/lua/bindings.lua"))
                .exec()?;
            ctx.load(include_str!("../../resources/lua/defaults.lua"))
                .exec()?;
            ctx.load(include_str!("../../resources/lua/lua_command.lua"))
                .exec()?;

            Ok(())
        })
        .unwrap();
    state
}

impl LuaScript {
    pub fn new(main_writer: Sender<Event>, dimensions: (u16, u16)) -> Self {
        Self {
            state: create_default_lua_state(main_writer.clone(), dimensions),
            writer: main_writer,
            on_connect_triggered: false,
            on_gmcp_ready_triggered: false,
        }
    }

    pub fn reset(&mut self, dimensions: (u16, u16)) {
        self.on_connect_triggered = false;
        self.on_gmcp_ready_triggered = false;
        self.state = create_default_lua_state(self.writer.clone(), dimensions);
    }

    pub fn get_output_lines(&self) -> Vec<Line> {
        self.state
            .context(|ctx| -> LuaResult<Vec<Line>> {
                let mut blight: BlightMud = ctx.globals().get("blight")?;
                let lines = blight.get_output_lines();
                ctx.globals().set("blight", blight)?;
                Ok(lines)
            })
            .unwrap()
    }

    pub fn check_for_alias_match(&self, input: &Line) -> bool {
        if !input.flags.bypass_script {
            let mut response = false;
            self.state.context(|ctx| {
                let alias_table: rlua::Table = ctx.globals().get(ALIAS_TABLE).unwrap();
                for pair in alias_table.pairs::<rlua::Value, rlua::AnyUserData>() {
                    let (_, alias) = pair.unwrap();
                    let rust_alias = &alias.borrow::<Alias>().unwrap();
                    let regex = &rust_alias.regex;
                    if rust_alias.enabled && regex.is_match(input.clean_line()) {
                        let cb: rlua::Function = alias.get_user_value().unwrap();
                        let captures: Vec<String> = regex
                            .captures(input.clean_line())
                            .unwrap()
                            .iter()
                            .map(|c| match c {
                                Some(m) => m.as_str().to_string(),
                                None => String::new(),
                            })
                            .collect();
                        if let Err(msg) = cb.call::<_, ()>(captures) {
                            output_stack_trace(&self.writer, &msg.to_string());
                        }
                        response = true;
                    }
                }
            });
            response
        } else {
            false
        }
    }

    pub fn check_for_trigger_match(&self, line: &mut Line) {
        self.check_trigger_match(line, TRIGGER_TABLE);
    }

    pub fn check_for_prompt_trigger_match(&self, line: &mut Line) {
        self.check_trigger_match(line, PROMPT_TRIGGER_TABLE);
    }

    fn check_trigger_match(&self, line: &mut Line, table: &str) {
        let input = line.clean_line().to_string();
        let raw_input = line.line().to_string();

        self.state.context(|ctx| {
            let trigger_table: rlua::Table = ctx.globals().get(table).unwrap();
            for pair in trigger_table.pairs::<rlua::Value, rlua::AnyUserData>() {
                let (_, trigger) = pair.unwrap();
                let rust_trigger = &trigger.borrow::<Trigger>().unwrap();

                let trigger_captures = if rust_trigger.raw {
                    rust_trigger.regex.captures(&raw_input)
                } else {
                    rust_trigger.regex.captures(&input)
                };

                if rust_trigger.enabled {
                    if let Some(caps) = trigger_captures {
                        let captures: Vec<String> = caps
                            .iter()
                            .map(|c| match c {
                                Some(m) => m.as_str().to_string(),
                                None => String::new(),
                            })
                            .collect();

                        let cb: rlua::Function = trigger.get_user_value().unwrap();
                        if let Err(msg) = cb.call::<_, ()>(captures) {
                            output_stack_trace(&self.writer, &msg.to_string());
                        }

                        line.flags.matched = true;
                        line.flags.gag =
                            rust_trigger.gag || ctx.globals().get(GAG_NEXT_TRIGGER_LINE).unwrap();

                        // Reset the gag flag
                        ctx.globals().set(GAG_NEXT_TRIGGER_LINE, false).unwrap();
                    }
                }
            }
        });
    }

    pub fn run_timed_function(&mut self, id: u32) {
        self.state
            .context(|ctx| -> Result<()> {
                let table: rlua::Table = ctx.globals().get(TIMED_FUNCTION_TABLE)?;
                let func: rlua::Function = table.get(id)?;
                func.call::<_, ()>(())?;
                Ok(())
            })
            .unwrap();
    }

    pub fn remove_timed_function(&mut self, id: u32) {
        self.state
            .context(|ctx| -> Result<()> {
                let table: rlua::Table = ctx.globals().get(TIMED_FUNCTION_TABLE)?;
                table.set(id, rlua::Nil)?;
                Ok(())
            })
            .unwrap();
    }

    pub fn receive_gmcp(&mut self, data: &str) {
        let split = data
            .splitn(2, ' ')
            .map(String::from)
            .collect::<Vec<String>>();
        let msg_type = &split[0];
        let content = &split[1];
        self.state
            .context(|ctx| {
                let listener_table: rlua::Table = ctx.globals().get(GMCP_LISTENER_TABLE).unwrap();
                if let Ok(func) = listener_table.get::<_, rlua::Function>(msg_type.clone()) {
                    if let Err(msg) = func.call::<_, ()>(content.clone()) {
                        output_stack_trace(&self.writer, &msg.to_string());
                    }
                }
                rlua::Result::Ok(())
            })
            .unwrap()
    }

    pub fn load_script(&mut self, path: &str) -> Result<()> {
        let mut file = File::open(path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        if let Err(msg) = self
            .state
            .context(|ctx| -> LuaResult<()> { ctx.load(&content).set_name(path)?.exec() })
        {
            output_stack_trace(&self.writer, &msg.to_string());
        }
        Ok(())
    }

    pub fn on_connect(&mut self, host: &str, port: u16) {
        if !self.on_connect_triggered {
            self.on_connect_triggered = true;
            self.state
                .context(|ctx| -> LuaResult<()> {
                    if let Ok(callback) = ctx
                        .globals()
                        .get::<_, rlua::Function>(ON_CONNCTION_CALLBACK)
                    {
                        if let Err(msg) = callback.call::<_, ()>((host, port)) {
                            output_stack_trace(&self.writer, &msg.to_string());
                        }
                    }
                    Ok(())
                })
                .unwrap();
        }
    }

    pub fn on_disconnect(&mut self) {
        self.state
            .context(|ctx| -> LuaResult<()> {
                if let Ok(callback) = ctx
                    .globals()
                    .get::<_, rlua::Function>(ON_DISCONNECT_CALLBACK)
                {
                    if let Err(msg) = callback.call::<_, ()>(()) {
                        output_stack_trace(&self.writer, &msg.to_string());
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    pub fn set_dimensions(&mut self, dim: (u16, u16)) -> LuaResult<()> {
        self.state.context(|ctx| -> LuaResult<()> {
            let mut blight: BlightMud = ctx.globals().get("blight")?;
            blight.screen_dimensions = dim;
            ctx.globals().set("blight", blight)?;
            Ok(())
        })
    }

    pub fn on_gmcp_ready(&mut self) {
        if !self.on_gmcp_ready_triggered {
            self.on_gmcp_ready_triggered = true;
            self.state
                .context(|ctx| -> Result<(), rlua::Error> {
                    if let Ok(callback) = ctx
                        .globals()
                        .get::<_, rlua::Function>(ON_GMCP_READY_CALLBACK)
                    {
                        if let Err(msg) = callback.call::<_, ()>(()) {
                            output_stack_trace(&self.writer, &msg.to_string());
                        }
                    }
                    Ok(())
                })
                .unwrap();
        }
    }

    pub fn check_bindings(&mut self, cmd: &str) {
        self.state
            .context(|ctx| -> Result<(), rlua::Error> {
                let bind_table: rlua::Table = ctx.globals().get(COMMAND_BINDING_TABLE)?;
                if let Ok(callback) = bind_table.get::<_, rlua::Function>(cmd) {
                    callback.call::<_, ()>(())
                } else {
                    Ok(())
                }
            })
            .unwrap();
    }

    pub fn get_ui_events(&mut self) -> Vec<UiEvent> {
        self.state
            .context(|ctx| -> Result<Vec<UiEvent>, rlua::Error> {
                let mut blight: BlightMud = ctx.globals().get("blight")?;
                let events = blight.get_ui_events();
                ctx.globals().set("blight", blight)?;
                Ok(events)
            })
            .unwrap()
    }
}

#[cfg(test)]
mod lua_script_tests {
    use super::LuaScript;
    use crate::{
        event::Event,
        model::{Connection, Line},
        PROJECT_NAME, VERSION,
    };
    use rlua::Result as LuaResult;
    use std::sync::mpsc::{channel, Receiver, Sender};

    fn test_trigger(line: &str, lua: &LuaScript) -> bool {
        let mut line = Line::from(line);
        lua.check_for_trigger_match(&mut line);
        line.flags.matched
    }

    fn test_prompt_trigger(line: &str, lua: &LuaScript) -> bool {
        let mut line = Line::from(line);
        lua.check_for_prompt_trigger_match(&mut line);
        line.flags.matched
    }

    fn get_lua() -> (LuaScript, Receiver<Event>) {
        let (writer, reader): (Sender<Event>, Receiver<Event>) = channel();
        (LuaScript::new(writer, (80, 80)), reader)
    }

    #[test]
    fn test_lua_trigger() {
        let create_trigger_lua = r#"
        blight:add_trigger("^test$", {gag=true}, function () end)
        "#;

        let lua = get_lua().0;
        lua.state.context(|ctx| {
            ctx.load(create_trigger_lua).exec().unwrap();
        });

        assert!(test_trigger("test", &lua));
        assert!(!test_trigger("test test", &lua));
    }

    #[test]
    fn test_lua_prompt_trigger() {
        let create_prompt_trigger_lua = r#"
        blight:add_trigger("^test$", {prompt=true, gag=true}, function () end)
        "#;

        let lua = get_lua().0;
        lua.state.context(|ctx| {
            ctx.load(create_prompt_trigger_lua).exec().unwrap();
        });

        assert!(test_prompt_trigger("test", &lua));
        assert!(!test_prompt_trigger("test test", &lua));
    }

    #[test]
    fn test_lua_raw_trigger() {
        let create_trigger_lua = r#"
        blight:add_trigger("^\\x1b\\[31mtest\\x1b\\[0m$", {raw=true}, function () end)
        "#;

        let (lua, _reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load(create_trigger_lua).exec().unwrap();
        });

        assert!(test_trigger("\x1b[31mtest\x1b[0m", &lua));
        assert!(!test_trigger("test", &lua));
    }

    #[test]
    fn test_remove_trigger() {
        let lua = get_lua().0;
        let (ttrig, ptrig) = lua
            .state
            .context(|ctx| -> LuaResult<(u32, u32)> {
                let ttrig: u32 = ctx
                    .load(r#"return blight:add_trigger("^test$", {}, function () end)"#)
                    .call(())
                    .unwrap();
                let ptrig: u32 = ctx
                    .load(r#"return blight:add_trigger("^test$", {prompt=true}, function () end)"#)
                    .call(())
                    .unwrap();
                Ok((ttrig, ptrig))
            })
            .unwrap();

        assert!(test_trigger("test", &lua));
        assert!(test_prompt_trigger("test", &lua));

        lua.state.context(|ctx| {
            ctx.load(&format!("blight:remove_trigger({})", ttrig))
                .exec()
                .unwrap();
        });

        assert!(test_prompt_trigger("test", &lua));
        assert!(!test_trigger("test", &lua));

        lua.state.context(|ctx| {
            ctx.load(&format!("blight:remove_trigger({})", ptrig))
                .exec()
                .unwrap();
        });

        assert!(!test_trigger("test", &lua));
        assert!(!test_prompt_trigger("test", &lua));
    }

    #[test]
    fn test_lua_alias() {
        let create_alias_lua = r#"
        blight:add_alias("^test$", function () end)
        "#;

        let lua = get_lua().0;
        lua.state.context(|ctx| {
            ctx.load(create_alias_lua).exec().unwrap();
        });

        assert!(lua.check_for_alias_match(&Line::from("test")));
        assert!(!lua.check_for_alias_match(&Line::from(" test")));
    }

    #[test]
    fn test_lua_remove_alias() {
        let create_alias_lua = r#"
        return blight:add_alias("^test$", function () end)
        "#;

        let lua = get_lua().0;
        let index: i32 = lua
            .state
            .context(|ctx| ctx.load(create_alias_lua).call(()))
            .unwrap();

        assert!(lua.check_for_alias_match(&Line::from("test")));

        let delete_alias = format!("blight:remove_alias({})", index);
        lua.state.context(|ctx| {
            ctx.load(&delete_alias).exec().unwrap();
        });
        assert!(!lua.check_for_alias_match(&Line::from("test")));
    }

    #[test]
    fn test_dimensions() {
        let mut lua = get_lua().0;
        let dim: (u16, u16) = lua
            .state
            .context(|ctx| ctx.load("return blight:terminal_dimensions()").call(()))
            .unwrap();
        assert_eq!(dim, (80, 80));
        lua.set_dimensions((70, 70)).unwrap();
        let dim: (u16, u16) = lua
            .state
            .context(|ctx| ctx.load("return blight:terminal_dimensions()").call(()))
            .unwrap();
        assert_eq!(dim, (70, 70));
    }

    #[test]
    fn test_send_gmcp() {
        let send_gmcp_lua = r#"
        blight:send_gmcp("Core.Hello")
        "#;

        let (lua, reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load(send_gmcp_lua).exec().unwrap();
        });

        assert_eq!(reader.recv(), Ok(Event::GMCPSend("Core.Hello".to_string())));
    }

    #[test]
    fn test_version() {
        let lua = get_lua().0;
        let (name, version): (String, String) = lua
            .state
            .context(|ctx| -> LuaResult<(String, String)> {
                ctx.load("return blight:version()")
                    .call::<(), (String, String)>(())
            })
            .unwrap();
        assert_eq!(version, VERSION);
        assert_eq!(name, PROJECT_NAME);
    }

    fn assert_event(lua_code: &str, event: Event) {
        let (lua, reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load(lua_code).exec().unwrap();
        });

        assert_eq!(reader.recv(), Ok(event));
    }

    #[test]
    fn test_connect() {
        assert_event(
            "blight:connect(\"hostname\", 99)",
            Event::Connect(Connection {
                host: "hostname".to_string(),
                port: 99,
            }),
        );
    }

    #[test]
    fn test_output() {
        let (lua, _) = get_lua();
        lua.state.context(|ctx| {
            ctx.load("blight:output(\"test\", \"test\")")
                .exec()
                .unwrap();
        });
        assert_eq!(lua.get_output_lines(), vec![Line::from("test test")]);
    }

    #[test]
    fn test_load() {
        assert_event(
            "blight:load(\"/some/fancy/path\")",
            Event::LoadScript("/some/fancy/path".to_string()),
        );
    }

    #[test]
    fn test_reset() {
        assert_event("blight:reset()", Event::ResetScript);
    }

    #[test]
    fn test_sending() {
        assert_event(
            "blight:send(\"message\")",
            Event::ServerInput(Line::from("message")),
        );
    }

    #[test]
    fn test_send_bytes() {
        assert_event(
            "blight:send_bytes({ 0xff, 0xf1 })",
            Event::ServerInput(Line::from(&vec![0xff, 0xf1])),
        );
    }

    #[test]
    fn test_logging() {
        let (lua, reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load("blight:start_log(\"testworld\")").exec().unwrap();
            ctx.load("blight:stop_log()").exec().unwrap();
        });

        assert_eq!(
            reader.recv(),
            Ok(Event::StartLogging("testworld".to_string(), true))
        );
        assert_eq!(reader.recv(), Ok(Event::StopLogging));
    }

    #[test]
    fn test_conditional_gag() {
        let trigger = r#"
        blight:add_trigger("^Health (\\d+)$", {}, function (matches)
            if matches[2] == "100" then
                blight:gag()
            end
        end)
        "#;

        let (lua, _reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load(trigger).exec().unwrap();
        });

        let mut line = Line::from("Health 100");
        lua.check_for_trigger_match(&mut line);
        assert!(line.flags.gag);

        let mut line = Line::from("Health 10");
        lua.check_for_trigger_match(&mut line);
        assert!(!line.flags.gag);
    }

    fn check_color(lua: &LuaScript, output: &str, result: &str) {
        lua.state.context(|ctx| {
            ctx.load(&format!("blight:output({})", output))
                .exec()
                .unwrap();
        });
        assert_eq!(lua.get_output_lines()[0], Line::from(result));
    }

    #[test]
    fn test_color_output() {
        let (lua, _reader) = get_lua();
        check_color(
            &lua,
            "C_RED .. \"COLOR\" .. C_RESET",
            "\x1b[31mCOLOR\x1b[0m",
        );
        check_color(
            &lua,
            "C_GREEN .. \"COLOR\" .. C_RESET",
            "\x1b[32mCOLOR\x1b[0m",
        );
        check_color(
            &lua,
            "C_YELLOW .. \"COLOR\" .. C_RESET",
            "\x1b[33mCOLOR\x1b[0m",
        );
        check_color(
            &lua,
            "C_BLUE .. \"COLOR\" .. C_RESET",
            "\x1b[34mCOLOR\x1b[0m",
        );
        check_color(
            &lua,
            "C_MAGENTA .. \"COLOR\" .. C_RESET",
            "\x1b[35mCOLOR\x1b[0m",
        );
        check_color(
            &lua,
            "C_CYAN .. \"COLOR\" .. C_RESET",
            "\x1b[36mCOLOR\x1b[0m",
        );
        check_color(
            &lua,
            "C_WHITE .. \"COLOR\" .. C_RESET",
            "\x1b[37mCOLOR\x1b[0m",
        );
    }

    #[test]
    fn test_bindings() {
        let lua_code = r#"
        blight:bind("ctrl-a", function ()
            blight:output("ctrl-a")
        end)
        blight:bind("f1", function ()
            blight:output("f1")
        end)
        blight:bind("alt-1", function ()
            blight:output("alt-1")
        end)
        "#;

        let (mut lua, _reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load(lua_code).exec().unwrap();
        });

        lua.check_bindings("ctrl-a");
        assert_eq!(lua.get_output_lines(), [Line::from("ctrl-a")]);
        lua.check_bindings("alt-1");
        assert_eq!(lua.get_output_lines(), [Line::from("alt-1")]);
        lua.check_bindings("f1");
        assert_eq!(lua.get_output_lines(), [Line::from("f1")]);
        lua.check_bindings("ctrl-0");
        assert_eq!(lua.get_output_lines(), []);
    }

    #[test]
    fn test_on_connect_test() {
        let lua_code = r#"
        blight:on_connect(function ()
            blight:output("connected")
        end)
        "#;

        let (mut lua, _reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load(lua_code).exec().unwrap();
        });

        lua.on_connect("test", 21);
        assert_eq!(lua.get_output_lines(), [Line::from("connected")]);
        lua.on_connect("test", 21);
        assert_eq!(lua.get_output_lines(), []);
        lua.reset((100, 100));
        lua.state.context(|ctx| {
            ctx.load(lua_code).exec().unwrap();
        });
        lua.on_connect("test", 21);
        assert_eq!(lua.get_output_lines(), [Line::from("connected")]);
    }

    #[test]
    fn test_on_gmcp_ready_test() {
        let lua_code = r#"
        blight:on_gmcp_ready(function ()
            blight:output("gmcp")
        end)
        "#;

        let (mut lua, _reader) = get_lua();
        lua.state.context(|ctx| {
            ctx.load(lua_code).exec().unwrap();
        });

        lua.on_gmcp_ready();
        assert_eq!(lua.get_output_lines(), [Line::from("gmcp")]);
        lua.on_gmcp_ready();
        assert_eq!(lua.get_output_lines(), []);
        lua.reset((100, 100));
        lua.state.context(|ctx| {
            ctx.load(lua_code).exec().unwrap();
        });
        lua.on_gmcp_ready();
        assert_eq!(lua.get_output_lines(), [Line::from("gmcp")]);
    }
}
