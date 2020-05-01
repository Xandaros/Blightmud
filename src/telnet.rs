use crate::event::Event;
use crate::output_buffer::OutputBuffer;
use crate::session::Session;
use libtelnet_rs::{events::TelnetEvents, telnet::op_command as cmd, Parser};
use std::sync::{mpsc::Sender, Arc, Mutex};

pub struct TelnetHandler {
    parser: Arc<Mutex<Parser>>,
    main_thread_writer: Sender<Event>,
    output_buffer: OutputBuffer,
}

impl TelnetHandler {
    pub fn new(session: Session) -> Self {
        Self {
            parser: session.telnet_parser,
            main_thread_writer: session.main_thread_writer,
            output_buffer: OutputBuffer::new(),
        }
    }
}

impl TelnetHandler {
    pub fn parse(&mut self, data: &[u8]) {
        if let Ok(mut parser) = self.parser.lock() {
            for event in parser.receive(data) {
                match event {
                    TelnetEvents::IAC(iac) => {
                        if iac.command == cmd::GA {
                            self.output_buffer.buffer_to_prompt();
                            self.main_thread_writer
                                .send(Event::Prompt(self.output_buffer.prompt.clone()))
                                .unwrap();
                        }
                    }
                    TelnetEvents::Negotiation(_) => (),
                    TelnetEvents::Subnegotiation(_) => (),
                    TelnetEvents::DataSend(msg) => {
                        if !msg.is_empty() {
                            self.main_thread_writer
                                .send(Event::ServerSend(msg))
                                .unwrap();
                        }
                    }
                    TelnetEvents::DataReceive(msg) => {
                        if !msg.is_empty() {
                            let new_lines = self.output_buffer.receive(msg.as_slice());
                            self.main_thread_writer
                                .send(Event::Output(new_lines.join("\r\n")))
                                .unwrap();
                        }
                    }
                };
            }
        }
    }
}