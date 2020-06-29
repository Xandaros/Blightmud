use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct Map {
    rooms: Vec<Room>,
    current_room: Option<usize>,
}

#[allow(dead_code)]
impl Map {
    fn r#move(&mut self, dir: &str) {
        if let Some(current) = self.current_room {
            let current = &self.rooms[current];
            if let Some(id) = current.exits.get(dir) {
                self.current_room.replace(*id as usize);
            } else {
                self.current_room = self.add_room(dir);
            }
        }
    }

    fn add_room(&mut self, dir: &str) -> Option<usize> {
        if let Some(current) = self.current_room {
            let room = Room::create(self.rooms.len() as u64);
            self.rooms[current].add_exit(dir, room.id);
            self.rooms.push(room);
            Some(self.rooms.len())
        } else {
            None
        }
    }
}

#[derive(Debug, Default)]
struct Room {
    id: u64,
    name: String,
    exits: BTreeMap<String, u64>,
}

#[allow(dead_code)]
impl Room {
    fn create(id: u64) -> Self {
        Self {
            id,
            name: String::new(),
            exits: BTreeMap::new(),
        }
    }

    fn add_exit(&mut self, dir: &str, id: u64) {
        if !self.exits.contains_key(dir) {
            self.exits.insert(dir.to_string(), id);
        }
    }
}
