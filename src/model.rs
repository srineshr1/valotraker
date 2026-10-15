use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::catalog::{agent_name, map_name, queue_label, rank_short};

pub const MSG_VIEW: u32 = 0x8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Idle,
    Menus,
    Pregame,
    Ingame,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub agent: String,
    pub name: String,
    pub kd: String,
    pub hs: String,
    pub rank: String,
    pub tier: u8,
    pub rr: String,
    pub form: [i8; 5],
    pub you: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub phase: Phase,
    pub map: String,
    pub queue: String,
    pub timer_ends_ms: Option<u64>,
    pub allies: Vec<Row>,
    pub enemies: Vec<Row>,
    pub single_list: bool,
    pub note: String,
}

impl View {
    pub fn waiting() -> Self {
        Self {
            phase: Phase::Waiting,
            map: String::new(),
            queue: String::new(),
            timer_ends_ms: None,
            allies: Vec::new(),
            enemies: Vec::new(),
            single_list: false,
            note: String::new(),
        }
    }

    pub fn idle(phase: Phase) -> Self {
        Self {
            phase,
            ..Self::waiting()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub puuid: String,
    pub agent_id: String,
    pub ally: bool,
    pub you: bool,
}

#[derive(Clone, Debug)]
pub struct Roster {
    pub phase: Phase,
    pub map_id: String,
    pub queue_id: String,
    pub timer_ends_ms: Option<u64>,
    pub slots: Vec<Slot>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cached {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub name_at: u64,
    #[serde(default)]
    pub rank_at: u64,
    #[serde(default)]
    pub rank_ok: bool,
    #[serde(default)]
    pub tier: u8,
    #[serde(default)]
    pub rr: u16,
    #[serde(default)]
    pub placing: bool,
    #[serde(default)]
    pub form: Vec<i8>,
    #[serde(default)]
    pub kd_at: u64,
    #[serde(default)]
    pub kd_ok: bool,
    #[serde(default)]
    pub kd: Option<f32>,
    #[serde(default)]
    pub hs: Option<u8>,
    #[serde(default)]
    pub hs_ok: bool,
    #[serde(default)]
    pub match_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CacheFile {
    #[serde(default)]
    pub players: HashMap<String, Cached>,
}

pub struct Shared {
    pub view: Mutex<View>,
    pub hwnd: std::sync::atomic::AtomicIsize,
}

pub fn lk<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("valotraker")
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn cache_key(puuid: &str) -> String {
    puuid.to_ascii_lowercase()
}

pub fn need_rank(cached: &Cached, now: u64) -> bool {
    if cached.rank_at == 0 {
        return true;
    }
    let ttl = if cached.rank_ok { 600 } else { 90 };
    now.saturating_sub(cached.rank_at) >= ttl
}

pub fn need_kd(cached: &Cached, now: u64) -> bool {
    if cached.kd_at == 0 || (cached.kd_ok && !cached.hs_ok) {
        return true;
    }
    let ttl = if cached.kd_ok { 1800 } else { 90 };
    now.saturating_sub(cached.kd_at) >= ttl
}

pub fn need_name(cached: &Cached, now: u64) -> bool {
    if !cached.name.is_empty() && now.saturating_sub(cached.name_at) < 86_400 {
        return false;
    }
    cached.name_at == 0 || now.saturating_sub(cached.name_at) >= 60
}

pub fn compose(roster: &Roster, cache: &CacheFile) -> View {
    let mut allies = Vec::new();
    let mut enemies = Vec::new();
    for slot in &roster.slots {
        let row = row_for(slot, cache);
        if slot.ally {
            allies.push(row);
        } else {
            enemies.push(row);
        }
    }
    allies.sort_by_key(|row| !row.you);

    let mut single_list = false;
    if enemies.is_empty() && allies.len() > 5 {
        single_list = true;
    } else if allies.is_empty() && enemies.len() > 5 {
        allies = enemies;
        enemies = Vec::new();
        single_list = true;
    }

    View {
        phase: roster.phase,
        map: map_name(&roster.map_id),
        queue: queue_label(&roster.queue_id),
        timer_ends_ms: roster.timer_ends_ms,
        allies,
        enemies,
        single_list,
        note: String::new(),
    }
}

fn row_for(slot: &Slot, cache: &CacheFile) -> Row {
    let cached = cache.players.get(&cache_key(&slot.puuid));
    let (name, kd, hs, rank, tier, rr, form) = match cached {
        None => (
            "…".into(),
            "…".into(),
            "…".into(),
            "…".into(),
            0,
            String::new(),
            [0; 5],
        ),
        Some(cached) => (
            display_name(cached),
            display_kd(cached),
            display_hs(cached),
            display_rank(cached),
            if cached.rank_ok { cached.tier } else { 0 },
            display_rr(cached),
            display_form(cached),
        ),
    };
    Row {
        agent: agent_name(&slot.agent_id).into(),
        name,
        kd,
        hs,
        rank,
        tier,
        rr,
        form,
        you: slot.you,
    }
}

fn display_name(cached: &Cached) -> String {
    if !cached.name.is_empty() {
        if cached.tag.is_empty() {
            cached.name.clone()
        } else {
            format!("{}#{}", cached.name, cached.tag)
        }
    } else if cached.name_at == 0 {
        "…".into()
    } else {
        "Private".into()
    }
}

fn display_rank(cached: &Cached) -> String {
    if !cached.rank_ok || cached.rank_at == 0 {
        return "…".into();
    }
    if cached.placing {
        return "PLC".into();
    }
    if cached.tier == 0 {
        return "UR".into();
    }
    rank_short(cached.tier).into()
}

fn display_rr(cached: &Cached) -> String {
    if cached.rank_ok && !cached.placing && cached.tier > 0 {
        within_tier_rr(cached.tier, cached.rr).to_string()
    } else {
        String::new()
    }
}

fn within_tier_rr(tier: u8, rr: u16) -> u16 {
    if tier >= 3 {
        let base = u16::from(tier) * 100;
        if rr >= base {
            return rr - base;
        }
    }
    rr
}

fn display_kd(cached: &Cached) -> String {
    if cached.kd_at == 0 {
        return "…".into();
    }
    match cached.kd {
        Some(value) => format!("{value:.1}"),
        None => "—".into(),
    }
}

fn display_hs(cached: &Cached) -> String {
    if cached.kd_at == 0 || !cached.hs_ok {
        return "…".into();
    }
    match cached.hs {
        Some(value) => format!("{value}"),
        None => "—".into(),
    }
}

fn display_form(cached: &Cached) -> [i8; 5] {
    let mut form = [0i8; 5];
    let slice = if cached.form.len() > 5 {
        &cached.form[cached.form.len() - 5..]
    } else {
        cached.form.as_slice()
    };
    let start = 5 - slice.len();
    for (index, value) in slice.iter().enumerate() {
        form[start + index] = *value;
    }
    form
}

pub fn demo_view() -> View {
    let ids = [
        ("p1", "add6443a-41bd-e414-f6ad-e58d267f4e95", true),
        ("p2", "569fdd95-4d10-43ab-ca70-79becc718b46", true),
        ("p3", "8e253930-4c05-31dd-1b6c-968525494517", true),
        ("p4", "320b2a48-4d9b-a075-30f1-1f93a9b638fa", true),
        ("p5", "1e58de9c-4950-5125-93e9-a0aee9f98746", true),
        ("p6", "a3bfb853-43b2-7238-a4f1-ad90e9e46bcc", false),
        ("p7", "f94c3b30-42be-e959-889c-5aa313dba261", false),
        ("p8", "117ed9e3-49f3-6512-3ccf-0cada7e3823b", false),
        ("p9", "dade69b4-4f5a-8528-247b-219e5a1facd6", false),
        ("p10", "eb93336a-449b-9c1b-0a54-a891f7921d69", false),
    ];
    let roster = Roster {
        phase: Phase::Pregame,
        map_id: "7eaecc1b-4337-bbf6-6ab9-04b8f06b3319".into(),
        queue_id: "competitive".into(),
        timer_ends_ms: Some(now_ms() + 52_000),
        slots: ids
            .iter()
            .map(|(puuid, agent, ally)| Slot {
                puuid: (*puuid).into(),
                agent_id: (*agent).into(),
                ally: *ally,
                you: *puuid == "p1",
            })
            .collect(),
    };
    let samples = [
        ("p1", "you", "val", 19u8, 61u16, 1.18f32, Some(22u8), vec![1, -1, 1, 1, -1]),
        ("p2", "mist", "note", 21, 14, 0.92, Some(18), vec![-1, 1, 1, -1, 1]),
        ("p3", "quiet", "one", 17, 88, 1.05, Some(27), vec![1, 1, 1, -1, 1]),
        ("p4", "lineart", "draw", 13, 33, 0.84, Some(14), vec![-1, -1, 1, 1, -1]),
        ("p5", "setup", "kj", 18, 9, 1.31, Some(31), vec![1, -1, 1, 1, 1]),
        ("p6", "flash", "ace", 24, 40, 1.42, Some(24), vec![1, 1, 1, -1, 1]),
        ("p7", "veryLongPlayerName", "NA1", 20, 77, 1.11, Some(19), vec![-1, 1, -1, 1, 1]),
        ("p8", "cam", "wire", 22, 22, 0.97, Some(16), vec![1, -1, -1, 1, 1]),
        ("p9", "night", "owl", 0, 0, 0.0, None, vec![]),
        ("p10", "hot", "hand", 11, 51, 0.73, Some(11), vec![-1, -1, -1, 1, -1]),
    ];
    let mut cache = CacheFile::default();
    let now = now_secs();
    for (puuid, name, tag, tier, rr, kd, hs, form) in samples {
        let unranked = tier == 0;
        cache.players.insert(
            puuid.into(),
            Cached {
                name: name.into(),
                tag: tag.into(),
                name_at: now,
                rank_at: now,
                rank_ok: true,
                tier,
                rr,
                placing: false,
                form,
                kd_at: now,
                kd_ok: true,
                kd: if unranked { None } else { Some(kd) },
                hs: if unranked { None } else { hs },
                hs_ok: true,
                match_ids: Vec::new(),
            },
        );
    }
    compose(&roster, &cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_player_stays_blank() {
        let roster = Roster {
            phase: Phase::Pregame,
            map_id: "/Game/Maps/Ascent/Ascent".into(),
            queue_id: "competitive".into(),
            timer_ends_ms: None,
            slots: vec![Slot {
                puuid: "ABC".into(),
                agent_id: String::new(),
                ally: true,
                you: true,
            }],
        };
        let view = compose(&roster, &CacheFile::default());
        assert_eq!(view.map, "Ascent");
        assert_eq!(view.queue, "COMP");
        assert_eq!(view.allies[0].agent, "Pick");
        assert_eq!(view.allies[0].name, "…");
        assert_eq!(view.allies[0].rank, "…");
        assert!(view.allies[0].you);
    }

    #[test]
    fn ranked_cache_formats_the_row() {
        let mut cache = CacheFile::default();
        cache.players.insert(
            "abc".into(),
            Cached {
                name: "Ada".into(),
                tag: "one".into(),
                name_at: 10,
                rank_at: 10,
                rank_ok: true,
                tier: 19,
                rr: 54,
                placing: false,
                form: vec![1, -1],
                kd_at: 10,
                kd_ok: true,
                kd: Some(1.24),
                hs: Some(28),
                hs_ok: true,
                match_ids: Vec::new(),
            },
        );
        let roster = Roster {
            phase: Phase::Ingame,
            map_id: "7eaecc1b-4337-bbf6-6ab9-04b8f06b3319".into(),
            queue_id: "unrated".into(),
            timer_ends_ms: None,
            slots: vec![Slot {
                puuid: "ABC".into(),
                agent_id: "add6443a-41bd-e414-f6ad-e58d267f4e95".into(),
                ally: false,
                you: false,
            }],
        };
        let view = compose(&roster, &cache);
        assert_eq!(view.enemies[0].name, "Ada#one");
        assert_eq!(view.enemies[0].agent, "Jett");
        assert_eq!(view.enemies[0].rank, "D2");
        assert_eq!(view.enemies[0].rr, "54");
        assert_eq!(view.enemies[0].kd, "1.2");
        assert_eq!(view.enemies[0].hs, "28");
        assert_eq!(view.enemies[0].form, [0, 0, 0, 1, -1]);
        assert_eq!(view.queue, "UNR");
        assert_eq!(within_tier_rr(6, 676), 76);
        assert_eq!(within_tier_rr(11, 1131), 31);
        assert_eq!(within_tier_rr(24, 450), 450);
    }
}
