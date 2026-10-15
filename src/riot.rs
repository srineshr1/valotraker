use std::collections::HashSet;
use std::fs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::catalog::map_name;
use crate::log::log;
use crate::model::{
    cache_key, compose, data_dir, lk, need_kd, need_name, need_rank, now_ms, now_secs, CacheFile,
    Phase, Roster, Shared, Slot, View,
};
use crate::overlay;

const PLATFORM: &str = "ew0KCSJwbGF0Zm9ybVR5cGUiOiAiUEMiLA0KCSJwbGF0Zm9ybU9TIjogIldpbmRvd3MiLA0KCSJwbGF0Zm9ybU9TVmVyc2lvbiI6ICIxMC4wLjE5MDQyLjEuMjU2LjY0Yml0IiwNCgkicGxhdGZvcm1DaGlwc2V0IjogIlVua25vd24iDQp9";

pub fn run(shared: Arc<Shared>) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            log(&format!("runtime failed: {err}"));
            return;
        }
    };
    runtime.block_on(async move {
        if let Err(err) = app_loop(shared).await {
            log(&format!("stopped: {err}"));
        }
    });
}

struct Http {
    local: Client,
    remote: Client,
    limit: Arc<Semaphore>,
}

struct Meta {
    version: String,
    season_id: String,
    at: u64,
}

#[derive(Clone)]
struct Session {
    port: u16,
    password: String,
    protocol: String,
    puuid: String,
    access: String,
    entitlement: String,
    region: String,
    shard: String,
}

struct App {
    http: Http,
    shared: Arc<Shared>,
    session: Option<Session>,
    meta: Arc<Mutex<Meta>>,
    cache: Arc<Mutex<CacheFile>>,
    roster: Arc<Mutex<Option<Roster>>>,
    inflight: Arc<Mutex<HashSet<String>>>,
    resolved_region: Option<String>,
    last_error: String,
    last_phase: Phase,
    saw_waiting: bool,
}

async fn app_loop(shared: Arc<Shared>) -> Result<(), String> {
    let mut app = App::new(shared)?;
    app.load_meta_disk();
    loop {
        let wait = match app.tick().await {
            Ok(ms) => ms,
            Err(err) => {
                if err != app.last_error {
                    log(&err);
                    app.last_error = err;
                }
                app.session = None;
                app.resolved_region = None;
                3_000
            }
        };
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
}

impl App {
    fn new(shared: Arc<Shared>) -> Result<Self, String> {
        let local = Client::builder()
            .danger_accept_invalid_certs(true)
            .http1_only()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .pool_max_idle_per_host(1)
            .build()
            .map_err(|err| err.to_string())?;
        let remote = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(12))
            .pool_max_idle_per_host(2)
            .gzip(true)
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            http: Http {
                local,
                remote,
                limit: Arc::new(Semaphore::new(4)),
            },
            shared,
            session: None,
            meta: Arc::new(Mutex::new(Meta {
                version: String::new(),
                season_id: String::new(),
                at: 0,
            })),
            cache: Arc::new(Mutex::new(load_cache())),
            roster: Arc::new(Mutex::new(None)),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            resolved_region: None,
            last_error: String::new(),
            last_phase: Phase::Waiting,
            saw_waiting: false,
        })
    }

    fn load_meta_disk(&mut self) {
        let path = data_dir().join("meta.json");
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let Ok(saved) = serde_json::from_str::<SavedMeta>(&text) else {
            return;
        };
        if now_secs().saturating_sub(saved.at) < 6 * 3600
            && !saved.version.is_empty()
            && !saved.season_id.is_empty()
        {
            let mut meta = lk(&self.meta);
            meta.version = saved.version;
            meta.season_id = saved.season_id;
            meta.at = saved.at;
        }
    }

    async fn tick(&mut self) -> Result<u64, String> {
        let lock = match read_lockfile() {
            Ok(lock) => lock,
            Err(_) => {
                self.session = None;
                self.resolved_region = None;
                *lk(&self.roster) = None;
                if !self.saw_waiting {
                    log("waiting for Riot Client");
                    self.saw_waiting = true;
                }
                publish(&self.shared, View::waiting());
                return Ok(3_000);
            }
        };
        self.saw_waiting = false;
        let same = self
            .session
            .as_ref()
            .map(|session| session.port == lock.port && session.password == lock.password)
            .unwrap_or(false);
        if !same {
            self.session = None;
            self.resolved_region = None;
        }
        if self.session.is_none() {
            let session = self.login(&lock).await?;
            log(&format!(
                "signed in as {} on {}",
                short_id(&session.puuid),
                session.region
            ));
            self.session = Some(session);
        }
        let session = self.refresh_auth(self.session.clone().unwrap()).await?;
        self.session = Some(session.clone());

        let presence = match self.presence(&session).await {
            Ok(presence) => presence,
            Err(err) => {
                self.note_once(&format!("presence: {err}"));
                publish(&self.shared, View::idle(Phase::Idle));
                return Ok(2_000);
            }
        };
        if let Some((region, shard)) = affinity(&presence.region) {
            let mut session = session;
            session.region = region.into();
            session.shard = shard.into();
            self.session = Some(session);
        }
        let session = self.session.clone().unwrap();
        if presence.phase != self.last_phase {
            log(&format!(
                "phase {} {} {}",
                phase_name(presence.phase),
                presence.queue,
                presence.map
            ));
            self.last_phase = presence.phase;
        }

        match presence.phase {
            Phase::Menus | Phase::Idle | Phase::Waiting => {
                *lk(&self.roster) = None;
                publish(&self.shared, View::idle(presence.phase));
                Ok(1_500)
            }
            phase => {
                if let Err(err) = self.ensure_meta().await {
                    self.note_once(&format!("meta: {err}"));
                }
                match self.load_roster(&session, phase, &presence).await {
                    Ok(roster) => {
                        *lk(&self.roster) = Some(roster);
                        self.publish_current();
                        self.batch_names(&session).await;
                        self.kick_enrich(&session);
                        Ok(if phase == Phase::Pregame { 1_200 } else { 4_000 })
                    }
                    Err(err) => {
                        self.note_once(&format!("match: {err}"));
                        if lk(&self.roster).is_some() {
                            Ok(1_500)
                        } else {
                            let mut view = View::idle(phase);
                            view.note = "Couldn't read the lobby".into();
                            view.map = map_name(&presence.map);
                            view.queue = crate::catalog::queue_label(&presence.queue);
                            publish(&self.shared, view);
                            Ok(2_000)
                        }
                    }
                }
            }
        }
    }

    fn note_once(&mut self, msg: &str) {
        if msg != self.last_error {
            log(msg);
            self.last_error = msg.to_string();
        }
    }

    fn publish_current(&self) {
        let roster = lk(&self.roster);
        let Some(roster) = roster.as_ref() else {
            return;
        };
        let cache = lk(&self.cache);
        publish(&self.shared, compose(roster, &cache));
    }

    async fn login(&self, lock: &Lockfile) -> Result<Session, String> {
        let basic = basic_auth(&lock.password);
        let url = format!("{}://127.0.0.1:{}/entitlements/v1/token", lock.protocol, lock.port);
        let ent: Entitlements = self.local_json(&url, &basic).await?;
        let region_url = format!(
            "{}://127.0.0.1:{}/riotclient/region-locale",
            lock.protocol, lock.port
        );
        let region = self
            .local_json::<RegionLocale>(&region_url, &basic)
            .await
            .map(|locale| locale.region)
            .unwrap_or_else(|_| "na".into());
        let region = if region.is_empty() {
            "na".into()
        } else {
            region.to_ascii_lowercase()
        };
        Ok(Session {
            port: lock.port,
            password: lock.password.clone(),
            protocol: lock.protocol.clone(),
            puuid: ent.subject,
            access: ent.access_token,
            entitlement: ent.token,
            shard: shard_for(&region),
            region,
        })
    }

    async fn refresh_auth(&self, mut session: Session) -> Result<Session, String> {
        let basic = basic_auth(&session.password);
        let url = format!(
            "{}://127.0.0.1:{}/entitlements/v1/token",
            session.protocol, session.port
        );
        let ent: Entitlements = self.local_json(&url, &basic).await?;
        session.access = ent.access_token;
        session.entitlement = ent.token;
        if !ent.subject.is_empty() {
            session.puuid = ent.subject;
        }
        Ok(session)
    }

    async fn presence(&self, session: &Session) -> Result<LivePresence, String> {
        let basic = basic_auth(&session.password);
        let url = format!(
            "{}://127.0.0.1:{}/chat/v4/presences",
            session.protocol, session.port
        );
        let body = self.local_text(&url, &basic).await?;
        let parsed: Presences = serde_json::from_str(&body).map_err(|err| err.to_string())?;
        let mine = parsed.presences.into_iter().find(|presence| {
            presence.product.eq_ignore_ascii_case("valorant")
                && presence.puuid.eq_ignore_ascii_case(&session.puuid)
        });
        let Some(mine) = mine else {
            return Ok(LivePresence {
                phase: Phase::Idle,
                region: String::new(),
                map: String::new(),
                queue: String::new(),
            });
        };
        let private = mine
            .private
            .as_deref()
            .map(decode_private)
            .transpose()?
            .unwrap_or_default();
        Ok(LivePresence {
            phase: phase_of(&private.session_loop_state),
            region: mine.region,
            map: private.match_map,
            queue: private.queue_id,
        })
    }

    async fn ensure_meta(&self) -> Result<(), String> {
        {
            let meta = lk(&self.meta);
            if !meta.version.is_empty()
                && !meta.season_id.is_empty()
                && now_secs().saturating_sub(meta.at) < 6 * 3600
            {
                return Ok(());
            }
        }
        let version = self
            .http
            .remote
            .get("https://valorant-api.com/v1/version")
            .timeout(Duration::from_secs(6))
            .send()
            .await
            .map_err(|err| err.to_string())?
            .error_for_status()
            .map_err(|err| err.to_string())?
            .json::<VersionResp>()
            .await
            .map_err(|err| err.to_string())?;
        let seasons = self
            .http
            .remote
            .get("https://valorant-api.com/v1/seasons")
            .timeout(Duration::from_secs(6))
            .send()
            .await
            .map_err(|err| err.to_string())?
            .error_for_status()
            .map_err(|err| err.to_string())?
            .json::<SeasonsResp>()
            .await
            .map_err(|err| err.to_string())?;
        let season_id = active_act(&seasons.data).unwrap_or_default();
        let saved = SavedMeta {
            version: version.data.riot_client_version,
            season_id,
            at: now_secs(),
        };
        {
            let mut meta = lk(&self.meta);
            meta.version = saved.version.clone();
            meta.season_id = saved.season_id.clone();
            meta.at = saved.at;
        }
        let dir = data_dir();
        let _ = fs::create_dir_all(&dir);
        if let Ok(text) = serde_json::to_string(&saved) {
            let _ = fs::write(dir.join("meta.json"), text);
        }
        Ok(())
    }

    async fn load_roster(
        &mut self,
        session: &Session,
        phase: Phase,
        presence: &LivePresence,
    ) -> Result<Roster, String> {
        let regions = candidate_regions(self.resolved_region.as_deref(), session, presence);
        let mut last_missing = true;
        let mut last_err = String::from("match not found");
        for region in regions {
            let shard = shard_for(&region);
            let result = if phase == Phase::Pregame {
                self.fetch_pregame(session, &region, &shard, presence).await
            } else {
                self.fetch_core(session, &region, &shard, presence).await
            };
            match result {
                Ok(roster) => {
                    self.resolved_region = Some(region);
                    return Ok(roster);
                }
                Err(FetchErr::Missing) => {
                    last_missing = true;
                }
                Err(FetchErr::Other(err)) if http_rejection(&err) => {
                    last_missing = false;
                    last_err = err;
                    break;
                }
                Err(FetchErr::Other(_)) => {
                    last_missing = true;
                }
            }
        }
        self.resolved_region = None;
        if last_missing {
            Err("lobby not ready".into())
        } else {
            Err(last_err)
        }
    }

    async fn fetch_pregame(
        &self,
        session: &Session,
        region: &str,
        shard: &str,
        presence: &LivePresence,
    ) -> Result<Roster, FetchErr> {
        let base = format!("https://glz-{region}-1.{shard}.a.pvp.net");
        let player_url = format!("{base}/pregame/v1/players/{}", session.puuid);
        let player: PlayerMatch = self.riot_json(session, &player_url).await?;
        if player.match_id.is_empty() {
            return Err(FetchErr::Missing);
        }
        let match_url = format!("{base}/pregame/v1/matches/{}", player.match_id);
        let body = self.riot_text(session, &match_url).await?;
        parse_pregame(&body, &session.puuid, &presence.map, &presence.queue)
            .map_err(FetchErr::Other)
    }

    async fn fetch_core(
        &self,
        session: &Session,
        region: &str,
        shard: &str,
        presence: &LivePresence,
    ) -> Result<Roster, FetchErr> {
        let base = format!("https://glz-{region}-1.{shard}.a.pvp.net");
        let player_url = format!("{base}/core-game/v1/players/{}", session.puuid);
        let player: PlayerMatch = self.riot_json(session, &player_url).await?;
        if player.match_id.is_empty() {
            return Err(FetchErr::Missing);
        }
        let match_url = format!("{base}/core-game/v1/matches/{}", player.match_id);
        let body = self.riot_text(session, &match_url).await?;
        parse_core(&body, &session.puuid, &presence.map, &presence.queue).map_err(FetchErr::Other)
    }

    async fn batch_names(&self, session: &Session) {
        let now = now_secs();
        let wanted: Vec<String> = {
            let roster = lk(&self.roster);
            let cache = lk(&self.cache);
            let Some(roster) = roster.as_ref() else {
                return;
            };
            roster
                .slots
                .iter()
                .filter(|slot| {
                    cache
                        .players
                        .get(&cache_key(&slot.puuid))
                        .map(|cached| need_name(cached, now))
                        .unwrap_or(true)
                })
                .map(|slot| slot.puuid.clone())
                .collect()
        };
        if wanted.is_empty() {
            return;
        }
        let url = format!(
            "https://pd.{}.a.pvp.net/name-service/v2/players",
            session.shard
        );
        let names = match self.riot_post_json::<Vec<NameEntry>>(session, &url, &wanted).await {
            Ok(names) => names,
            Err(err) => {
                log(&format!("names: {err}"));
                return;
            }
        };
        {
            let mut cache = lk(&self.cache);
            let mut seen = HashSet::new();
            for entry in names {
                let key = cache_key(&entry.subject);
                seen.insert(key.clone());
                let cached = cache.players.entry(key).or_default();
                cached.name = entry.game_name;
                cached.tag = entry.tag_line;
                cached.name_at = now;
            }
            for puuid in &wanted {
                let key = cache_key(puuid);
                if !seen.contains(&key) {
                    let cached = cache.players.entry(key).or_default();
                    cached.name_at = now;
                }
            }
            save_cache(&cache);
        }
        self.publish_current();
    }

    fn kick_enrich(&self, session: &Session) {
        let now = now_secs();
        let puuids: Vec<String> = {
            let roster = lk(&self.roster);
            let cache = lk(&self.cache);
            let Some(roster) = roster.as_ref() else {
                return;
            };
            roster
                .slots
                .iter()
                .filter(|slot| {
                    let key = cache_key(&slot.puuid);
                    match cache.players.get(&key) {
                        None => true,
                        Some(cached) => need_rank(cached, now) || need_kd(cached, now),
                    }
                })
                .map(|slot| slot.puuid.clone())
                .collect()
        };
        for puuid in puuids {
            let key = cache_key(&puuid);
            {
                let mut inflight = lk(&self.inflight);
                if !inflight.insert(key.clone()) {
                    continue;
                }
            }
            let http = self.http.remote.clone();
            let limit = self.http.limit.clone();
            let session = session.clone();
            let meta = self.meta.clone();
            let cache = self.cache.clone();
            let roster = self.roster.clone();
            let shared = self.shared.clone();
            let inflight = self.inflight.clone();
            tokio::spawn(async move {
                let _guard = InflightGuard {
                    set: inflight,
                    key: key.clone(),
                };
                enrich_player(http, limit, session, meta, cache, roster, shared, puuid).await;
            });
        }
    }

    async fn local_text(&self, url: &str, basic: &str) -> Result<String, String> {
        let response = self
            .http
            .local
            .get(url)
            .header(AUTHORIZATION, basic)
            .send()
            .await
            .map_err(|err| format!("local connect failed ({err})"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("local {status}"));
        }
        Ok(text)
    }

    async fn local_json<T: for<'de> Deserialize<'de>>(&self, url: &str, basic: &str) -> Result<T, String> {
        let text = self.local_text(url, basic).await?;
        serde_json::from_str(&text).map_err(|err| err.to_string())
    }

    async fn riot_text(&self, session: &Session, url: &str) -> Result<String, FetchErr> {
        remote_text(&self.http.remote, &self.http.limit, session, &self.meta, url).await
    }

    async fn riot_json<T: for<'de> Deserialize<'de>>(
        &self,
        session: &Session,
        url: &str,
    ) -> Result<T, FetchErr> {
        let text = self.riot_text(session, url).await?;
        serde_json::from_str(&text).map_err(|err| FetchErr::Other(err.to_string()))
    }

    async fn riot_post_json<T: for<'de> Deserialize<'de>>(
        &self,
        session: &Session,
        url: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, String> {
        let _permit = self
            .http
            .limit
            .acquire()
            .await
            .map_err(|_| "stopped".to_string())?;
        let headers = riot_headers(session, &self.meta)?;
        let response = self
            .http
            .remote
            .put(url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("{status}"));
        }
        response.json().await.map_err(|err| err.to_string())
    }
}

struct InflightGuard {
    set: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        lk(&self.set).remove(&self.key);
    }
}

async fn enrich_player(
    http: Client,
    limit: Arc<Semaphore>,
    session: Session,
    meta: Arc<Mutex<Meta>>,
    cache: Arc<Mutex<CacheFile>>,
    roster: Arc<Mutex<Option<Roster>>>,
    shared: Arc<Shared>,
    puuid: String,
) {
    let key = cache_key(&puuid);
    let now = now_secs();
    let (rank_needed, kd_needed, known_matches) = {
        let cache = lk(&cache);
        match cache.players.get(&key) {
            None => (true, true, Vec::new()),
            Some(cached) => (
                need_rank(cached, now),
                need_kd(cached, now),
                cached.match_ids.clone(),
            ),
        }
    };
    if rank_needed {
        match fetch_rank(&http, &limit, &session, &meta, &puuid).await {
            Ok(rank) => {
                {
                    let mut cache = lk(&cache);
                    let cached = cache.players.entry(key.clone()).or_default();
                    cached.rank_at = now_secs();
                    cached.rank_ok = true;
                    cached.tier = rank.tier;
                    cached.rr = rank.rr;
                    cached.placing = rank.placing;
                    cached.form = rank.form;
                    if !rank.match_ids.is_empty() {
                        cached.match_ids = rank.match_ids.clone();
                    }
                    save_cache(&cache);
                }
                republish(&shared, &roster, &cache);
            }
            Err(err) => {
                log(&format!("rank {}: {err}", short_id(&puuid)));
                let mut cache = lk(&cache);
                let cached = cache.players.entry(key.clone()).or_default();
                cached.rank_at = now_secs();
                cached.rank_ok = false;
            }
        }
    }
    if kd_needed {
        let ids = {
            let cache = lk(&cache);
            cache
                .players
                .get(&key)
                .map(|cached| cached.match_ids.clone())
                .filter(|ids| !ids.is_empty())
                .unwrap_or(known_matches)
        };
        match fetch_kd(&http, &limit, &session, &meta, &puuid, &ids).await {
            Ok(combat) => {
                {
                    let mut cache = lk(&cache);
                    let cached = cache.players.entry(key).or_default();
                    cached.kd_at = now_secs();
                    cached.kd_ok = true;
                    cached.kd = combat.kd;
                    cached.hs = combat.hs;
                    cached.hs_ok = true;
                    save_cache(&cache);
                }
                republish(&shared, &roster, &cache);
            }
            Err(err) => {
                log(&format!("kd {}: {err}", short_id(&puuid)));
                let mut cache = lk(&cache);
                let cached = cache.players.entry(key).or_default();
                cached.kd_at = now_secs();
                cached.kd_ok = false;
            }
        }
    }
}

struct RankInfo {
    tier: u8,
    rr: u16,
    placing: bool,
    form: Vec<i8>,
    match_ids: Vec<String>,
}

async fn fetch_rank(
    http: &Client,
    limit: &Semaphore,
    session: &Session,
    meta: &Mutex<Meta>,
    puuid: &str,
) -> Result<RankInfo, String> {
    let season = lk(meta).season_id.clone();
    let mmr_url = format!("https://pd.{}.a.pvp.net/mmr/v1/players/{puuid}", session.shard);
    let mmr: Mmr = remote_json(http, limit, session, meta, &mmr_url).await?;
    let updates_url = format!(
        "https://pd.{}.a.pvp.net/mmr/v1/players/{puuid}/competitiveupdates?startIndex=0&endIndex=5",
        session.shard
    );
    let updates: CompUpdates = remote_json(http, limit, session, meta, &updates_url)
        .await
        .unwrap_or(CompUpdates { matches: Vec::new() });
    let (tier, rr, placing) = read_rank(&mmr, &season);
    let (form, match_ids) = read_form(&updates, &season);
    Ok(RankInfo {
        tier,
        rr,
        placing,
        form,
        match_ids,
    })
}

struct Combat {
    kd: Option<f32>,
    hs: Option<u8>,
}

async fn fetch_kd(
    http: &Client,
    limit: &Semaphore,
    session: &Session,
    meta: &Mutex<Meta>,
    puuid: &str,
    known_ids: &[String],
) -> Result<Combat, String> {
    let ids = if known_ids.is_empty() {
        let season = lk(meta).season_id.clone();
        let updates_url = format!(
            "https://pd.{}.a.pvp.net/mmr/v1/players/{puuid}/competitiveupdates?startIndex=0&endIndex=4",
            session.shard
        );
        let updates: CompUpdates = remote_json(http, limit, session, meta, &updates_url).await?;
        let (_, ids) = read_form(&updates, &season);
        ids
    } else {
        known_ids.iter().take(4).cloned().collect()
    };
    if ids.is_empty() {
        return Ok(Combat { kd: None, hs: None });
    }
    let mut kills = 0u32;
    let mut deaths = 0u32;
    let mut games = 0u32;
    let mut head = 0u32;
    let mut body = 0u32;
    let mut leg = 0u32;
    for id in ids.iter().take(4) {
        let url = format!(
            "https://pd.{}.a.pvp.net/match-details/v1/matches/{id}",
            session.shard
        );
        let details: MatchDetails = match remote_json(http, limit, session, meta, &url).await {
            Ok(details) => details,
            Err(_) => continue,
        };
        if let Some(player) = details
            .players
            .iter()
            .find(|player| player.subject.eq_ignore_ascii_case(puuid))
        {
            if let Some(stats) = &player.stats {
                kills += stats.kills;
                deaths += stats.deaths;
                games += 1;
            }
        }
        let (round_head, round_body, round_leg) = shots_for(&details, puuid);
        head += round_head;
        body += round_body;
        leg += round_leg;
    }
    if games == 0 {
        return Ok(Combat { kd: None, hs: None });
    }
    let kd = if deaths == 0 {
        kills as f32
    } else {
        kills as f32 / deaths as f32
    };
    Ok(Combat {
        kd: Some(kd),
        hs: shot_percent(head, body, leg),
    })
}

fn shots_for(details: &MatchDetails, puuid: &str) -> (u32, u32, u32) {
    let mut head = 0u32;
    let mut body = 0u32;
    let mut leg = 0u32;
    for round in &details.rounds {
        for player in &round.players {
            if !player.subject.eq_ignore_ascii_case(puuid) {
                continue;
            }
            for hit in &player.damage {
                head += hit.headshots;
                body += hit.bodyshots;
                leg += hit.legshots;
            }
        }
    }
    (head, body, leg)
}

fn shot_percent(head: u32, body: u32, leg: u32) -> Option<u8> {
    let total = head.saturating_add(body).saturating_add(leg);
    if total == 0 {
        return None;
    }
    Some(((u64::from(head) * 100) / u64::from(total)) as u8)
}

fn read_rank(mmr: &Mmr, season: &str) -> (u8, u16, bool) {
    let Some(skills) = mmr.queue_skills.as_ref() else {
        return (0, 0, false);
    };
    let Some(comp) = skills.competitive.as_ref() else {
        return (0, 0, false);
    };
    let Some(seasons) = comp.seasonal.as_ref() else {
        return (0, 0, false);
    };
    let info = if !season.is_empty() {
        seasons.get(season)
    } else {
        seasons.values().max_by_key(|info| info.games)
    };
    let Some(info) = info else {
        return (0, 0, false);
    };
    if info.needed > 0 {
        return (0, 0, true);
    }
    (info.tier.min(255) as u8, info.rr.min(10_000) as u16, false)
}

fn read_form(updates: &CompUpdates, season: &str) -> (Vec<i8>, Vec<String>) {
    let filtered: Vec<&CompMatch> = if season.is_empty() {
        updates.matches.iter().take(5).collect()
    } else {
        updates
            .matches
            .iter()
            .filter(|item| item.season_id == season || item.season_id.is_empty())
            .take(5)
            .collect()
    };
    let mut form: Vec<i8> = filtered
        .iter()
        .map(|item| {
            if item.rr_earned > 0 {
                1
            } else if item.rr_earned < 0 {
                -1
            } else {
                2
            }
        })
        .collect();
    form.reverse();
    let ids = filtered
        .iter()
        .filter(|item| !item.match_id.is_empty())
        .map(|item| item.match_id.clone())
        .take(4)
        .collect();
    (form, ids)
}

fn republish(shared: &Shared, roster: &Mutex<Option<Roster>>, cache: &Mutex<CacheFile>) {
    let roster = lk(roster);
    let Some(roster) = roster.as_ref() else {
        return;
    };
    let cache = lk(cache);
    publish(shared, compose(roster, &cache));
}

async fn remote_json<T: for<'de> Deserialize<'de>>(
    http: &Client,
    limit: &Semaphore,
    session: &Session,
    meta: &Mutex<Meta>,
    url: &str,
) -> Result<T, String> {
    let text = remote_text(http, limit, session, meta, url)
        .await
        .map_err(|err| match err {
            FetchErr::Missing => "not found".into(),
            FetchErr::Other(err) => err,
        })?;
    serde_json::from_str(&text).map_err(|err| err.to_string())
}

async fn remote_text(
    http: &Client,
    limit: &Semaphore,
    session: &Session,
    meta: &Mutex<Meta>,
    url: &str,
) -> Result<String, FetchErr> {
    let _permit = limit.acquire().await.map_err(|_| FetchErr::Other("stopped".into()))?;
    let headers = riot_headers(session, meta).map_err(FetchErr::Other)?;
    let response = http
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|err| FetchErr::Other(err.to_string()))?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Err(FetchErr::Missing);
    }
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = text.chars().filter(|ch| *ch != '\n').take(160).collect();
        return Err(FetchErr::Other(format!("{status} {snippet}")));
    }
    Ok(text)
}

fn riot_headers(session: &Session, meta: &Mutex<Meta>) -> Result<HeaderMap, String> {
    let version = lk(meta).version.clone();
    if version.is_empty() {
        return Err("client version missing".into());
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", session.access)).map_err(|err| err.to_string())?,
    );
    headers.insert(
        "X-Riot-Entitlements-JWT",
        HeaderValue::from_str(&session.entitlement).map_err(|err| err.to_string())?,
    );
    headers.insert(
        "X-Riot-ClientPlatform",
        HeaderValue::from_static(PLATFORM),
    );
    headers.insert(
        "X-Riot-ClientVersion",
        HeaderValue::from_str(&version).map_err(|err| err.to_string())?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

fn publish(shared: &Shared, view: View) {
    let changed = {
        let mut current = lk(&shared.view);
        if *current == view {
            false
        } else {
            *current = view;
            true
        }
    };
    if changed {
        overlay::poke(shared.hwnd.load(Ordering::SeqCst));
    }
}

fn candidate_regions(resolved: Option<&str>, session: &Session, presence: &LivePresence) -> Vec<String> {
    let mut regions = Vec::new();
    let mut push = |value: &str| {
        let value = value.trim().to_ascii_lowercase();
        if !value.is_empty() && !regions.iter().any(|have| have == &value) {
            regions.push(value);
        }
    };
    if let Some(region) = resolved {
        push(region);
        return regions;
    }
    if let Some((region, _)) = affinity(&presence.region) {
        push(region);
    }
    if let Some((region, _)) = affinity(&session.region) {
        push(region);
    }
    for region in ["na", "eu", "ap", "kr", "latam", "br"] {
        push(region);
    }
    regions
}

fn basic_auth(password: &str) -> String {
    let raw = format!("riot:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

fn short_id(puuid: &str) -> String {
    puuid.chars().take(8).collect()
}

pub fn shard_for(region: &str) -> String {
    affinity(region)
        .map(|(_, shard)| shard.to_string())
        .unwrap_or_else(|| "na".into())
}

fn affinity(value: &str) -> Option<(&'static str, &'static str)> {
    let bare = value
        .trim()
        .to_ascii_lowercase()
        .trim_end_matches(|ch: char| ch.is_ascii_digit())
        .to_string();
    match bare.as_str() {
        "na" | "us" => Some(("na", "na")),
        "latam" | "la" => Some(("latam", "na")),
        "br" => Some(("br", "na")),
        "eu" => Some(("eu", "eu")),
        "ap" | "jp" | "asia" | "oc" | "oce" => Some(("ap", "ap")),
        "kr" => Some(("kr", "kr")),
        "pbe" => Some(("pbe", "na")),
        _ => None,
    }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Waiting => "waiting",
        Phase::Idle => "idle",
        Phase::Menus => "menus",
        Phase::Pregame => "pregame",
        Phase::Ingame => "ingame",
    }
}

fn http_rejection(err: &str) -> bool {
    err.as_bytes().first().is_some_and(|byte| byte.is_ascii_digit())
}

struct Lockfile {
    port: u16,
    password: String,
    protocol: String,
}

fn read_lockfile() -> Result<Lockfile, String> {
    let path = std::env::var("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("Riot Games")
        .join("Riot Client")
        .join("Config")
        .join("lockfile");
    let text = fs::read_to_string(path).map_err(|_| "no lockfile".to_string())?;
    parse_lockfile(&text)
}

fn parse_lockfile(text: &str) -> Result<Lockfile, String> {
    let text = text.trim().trim_start_matches('\u{feff}');
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 5 {
        return Err("bad lockfile".into());
    }
    let port = parts[2].parse::<u16>().map_err(|_| "bad port".to_string())?;
    let protocol = parts[parts.len() - 1].to_string();
    let password = parts[3..parts.len() - 1].join(":");
    if password.is_empty() || !(protocol == "https" || protocol == "http") {
        return Err("bad lockfile".into());
    }
    Ok(Lockfile {
        port,
        password,
        protocol,
    })
}

fn phase_of(state: &str) -> Phase {
    match state.to_ascii_uppercase().as_str() {
        "PREGAME" => Phase::Pregame,
        "INGAME" => Phase::Ingame,
        _ => Phase::Menus,
    }
}

fn decode_private(value: &str) -> Result<PrivatePresence, String> {
    let mut padded = value.trim().replace(['\n', '\r'], "");
    let extra = (4 - padded.len() % 4) % 4;
    padded.extend(std::iter::repeat('=').take(extra));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .map_err(|err| err.to_string())?;
    let mut private: PrivatePresence = serde_json::from_slice(&bytes).map_err(|err| err.to_string())?;
    if let Some(nested) = private.match_presence_data.take() {
        if !nested.session_loop_state.is_empty() {
            private.session_loop_state = nested.session_loop_state;
        }
        if !nested.match_map.is_empty() {
            private.match_map = nested.match_map;
        }
        if !nested.queue_id.is_empty() {
            private.queue_id = nested.queue_id;
        }
    }
    Ok(private)
}

struct LivePresence {
    phase: Phase,
    region: String,
    map: String,
    queue: String,
}

enum FetchErr {
    Missing,
    Other(String),
}

fn parse_pregame(body: &str, me: &str, presence_map: &str, presence_queue: &str) -> Result<Roster, String> {
    let parsed: PregameMatch = serde_json::from_str(body).map_err(|err| err.to_string())?;
    let teams = pregame_teams(&parsed);
    let ally_id = parsed
        .ally_team
        .as_ref()
        .map(|team| team.team_id.clone())
        .or_else(|| {
            teams.iter().find_map(|team| {
                team.players
                    .iter()
                    .any(|player| player.subject.eq_ignore_ascii_case(me))
                    .then(|| team.team_id.clone())
            })
        });
    let mut slots = Vec::new();
    for team in teams {
        let ally = ally_id
            .as_ref()
            .map(|id| id == &team.team_id)
            .unwrap_or(false);
        for player in team.players {
            if player.subject.is_empty() {
                continue;
            }
            slots.push(Slot {
                you: player.subject.eq_ignore_ascii_case(me),
                agent_id: player.character_id,
                ally: ally || player.subject.eq_ignore_ascii_case(me),
                puuid: player.subject,
            });
        }
    }
    if slots.is_empty() {
        return Err("empty lobby".into());
    }
    let map_id = if parsed.map_id.is_empty() {
        presence_map.to_string()
    } else {
        parsed.map_id
    };
    let queue_id = if parsed.queue_id.is_empty() {
        presence_queue.to_string()
    } else {
        parsed.queue_id
    };
    let timer_ends_ms = parsed.phase_time_ns.and_then(|ns| {
        if ns <= 0 {
            None
        } else {
            Some(now_ms() + (ns as u64) / 1_000_000)
        }
    });
    Ok(Roster {
        phase: Phase::Pregame,
        map_id,
        queue_id,
        timer_ends_ms,
        slots,
    })
}

fn pregame_teams(parsed: &PregameMatch) -> Vec<Team> {
    if let Some(teams) = &parsed.teams {
        if teams.iter().any(|team| !team.players.is_empty()) {
            return teams.clone();
        }
    }
    let mut teams = Vec::new();
    if let Some(team) = &parsed.ally_team {
        teams.push(team.clone());
    }
    if let Some(team) = &parsed.enemy_team {
        teams.push(team.clone());
    }
    teams
}

fn parse_core(body: &str, me: &str, presence_map: &str, presence_queue: &str) -> Result<Roster, String> {
    let parsed: CoreMatch = serde_json::from_str(body).map_err(|err| err.to_string())?;
    let my_team = parsed
        .players
        .iter()
        .find(|player| player.subject.eq_ignore_ascii_case(me))
        .map(|player| player.team_id.clone());
    let slots = parsed
        .players
        .into_iter()
        .filter(|player| !player.subject.is_empty())
        .map(|player| Slot {
            you: player.subject.eq_ignore_ascii_case(me),
            ally: my_team
                .as_ref()
                .map(|team| team == &player.team_id)
                .unwrap_or(false),
            agent_id: player.character_id,
            puuid: player.subject,
        })
        .collect::<Vec<_>>();
    if slots.is_empty() {
        return Err("empty match".into());
    }
    Ok(Roster {
        phase: Phase::Ingame,
        map_id: if parsed.map_id.is_empty() {
            presence_map.to_string()
        } else {
            parsed.map_id
        },
        queue_id: if parsed.queue_id.is_empty() {
            presence_queue.to_string()
        } else {
            parsed.queue_id
        },
        timer_ends_ms: None,
        slots,
    })
}

fn active_act(seasons: &[Season]) -> Option<String> {
    let now = iso_utc(now_secs() as i64);
    let mut best: Option<&Season> = None;
    for season in seasons {
        if season.kind.as_deref() != Some("act") {
            continue;
        }
        let start = season.start_time.as_deref().unwrap_or("");
        let end = season.end_time.as_deref().unwrap_or("");
        if stamp(start) <= stamp(&now) && (end.is_empty() || stamp(&now) < stamp(end)) {
            if best
                .map(|have| stamp(start) >= stamp(have.start_time.as_deref().unwrap_or("")))
                .unwrap_or(true)
            {
                best = Some(season);
            }
        }
    }
    best.map(|season| season.uuid.clone())
}

fn stamp(value: &str) -> &str {
    value.get(..19).unwrap_or(value)
}

fn iso_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let tod = unix.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (year + if month <= 2 { 1 } else { 0 }) as i32;
    (year, month, day)
}

fn load_cache() -> CacheFile {
    let path = data_dir().join("players.json");
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &CacheFile) {
    let mut copy = cache.clone();
    if copy.players.len() > 300 {
        let mut ranked: Vec<(String, u64)> = copy
            .players
            .iter()
            .map(|(key, cached)| {
                (
                    key.clone(),
                    cached.rank_at.max(cached.kd_at).max(cached.name_at),
                )
            })
            .collect();
        ranked.sort_by_key(|(_, at)| *at);
        let extra = ranked.len() - 300;
        for (key, _) in ranked.into_iter().take(extra) {
            copy.players.remove(&key);
        }
    }
    let dir = data_dir();
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("players.json");
    let tmp = dir.join("players.json.tmp");
    if let Ok(text) = serde_json::to_string(&copy) {
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

#[derive(Deserialize)]
struct Entitlements {
    #[serde(rename = "accessToken")]
    access_token: String,
    subject: String,
    token: String,
}

#[derive(Deserialize)]
struct RegionLocale {
    #[serde(default)]
    region: String,
}

#[derive(Deserialize)]
struct Presences {
    #[serde(default)]
    presences: Vec<PresenceItem>,
}

#[derive(Deserialize)]
struct PresenceItem {
    #[serde(default)]
    puuid: String,
    #[serde(default)]
    product: String,
    #[serde(default)]
    private: Option<String>,
    #[serde(default)]
    region: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PrivatePresence {
    #[serde(default)]
    session_loop_state: String,
    #[serde(default)]
    match_map: String,
    #[serde(default)]
    queue_id: String,
    #[serde(default)]
    match_presence_data: Option<MatchPresenceData>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MatchPresenceData {
    #[serde(default)]
    session_loop_state: String,
    #[serde(default)]
    match_map: String,
    #[serde(default)]
    queue_id: String,
}

#[derive(Deserialize)]
struct PlayerMatch {
    #[serde(rename = "MatchID", default)]
    match_id: String,
}

#[derive(Deserialize)]
struct PregameMatch {
    #[serde(rename = "MapID", default)]
    map_id: String,
    #[serde(rename = "QueueID", default)]
    queue_id: String,
    #[serde(rename = "PhaseTimeRemainingNS", default)]
    phase_time_ns: Option<i64>,
    #[serde(rename = "Teams", default)]
    teams: Option<Vec<Team>>,
    #[serde(rename = "AllyTeam", default)]
    ally_team: Option<Team>,
    #[serde(rename = "EnemyTeam", default)]
    enemy_team: Option<Team>,
}

#[derive(Clone, Deserialize)]
struct Team {
    #[serde(rename = "TeamID", default)]
    team_id: String,
    #[serde(rename = "Players", default)]
    players: Vec<PregamePlayer>,
}

#[derive(Clone, Deserialize)]
struct PregamePlayer {
    #[serde(rename = "Subject", default)]
    subject: String,
    #[serde(rename = "CharacterID", default)]
    character_id: String,
}

#[derive(Deserialize)]
struct CoreMatch {
    #[serde(rename = "MapID", default)]
    map_id: String,
    #[serde(rename = "QueueID", default)]
    queue_id: String,
    #[serde(rename = "Players", default)]
    players: Vec<CorePlayer>,
}

#[derive(Deserialize)]
struct CorePlayer {
    #[serde(rename = "Subject", default)]
    subject: String,
    #[serde(rename = "TeamID", default)]
    team_id: String,
    #[serde(rename = "CharacterID", default)]
    character_id: String,
}

#[derive(Deserialize)]
struct NameEntry {
    #[serde(rename = "Subject", default)]
    subject: String,
    #[serde(rename = "GameName", default)]
    game_name: String,
    #[serde(rename = "TagLine", default)]
    tag_line: String,
}

#[derive(Deserialize)]
struct Mmr {
    #[serde(rename = "QueueSkills", default)]
    queue_skills: Option<QueueSkills>,
}

#[derive(Deserialize)]
struct QueueSkills {
    #[serde(default)]
    competitive: Option<CompQueue>,
}

#[derive(Deserialize)]
struct CompQueue {
    #[serde(rename = "SeasonalInfoBySeasonID", default)]
    seasonal: Option<std::collections::HashMap<String, SeasonInfo>>,
}

#[derive(Deserialize, Default)]
struct SeasonInfo {
    #[serde(rename = "CompetitiveTier", default)]
    tier: u32,
    #[serde(rename = "RankedRating", default)]
    rr: u32,
    #[serde(rename = "GamesNeededForRating", default)]
    needed: u32,
    #[serde(rename = "NumberOfGames", default)]
    games: u32,
}

#[derive(Deserialize, Default)]
struct CompUpdates {
    #[serde(rename = "Matches", default)]
    matches: Vec<CompMatch>,
}

#[derive(Deserialize)]
struct CompMatch {
    #[serde(rename = "MatchID", default)]
    match_id: String,
    #[serde(rename = "SeasonID", default)]
    season_id: String,
    #[serde(rename = "RankedRatingEarned", default)]
    rr_earned: i32,
}

#[derive(Deserialize)]
struct MatchDetails {
    #[serde(default, alias = "Players")]
    players: Vec<MatchPlayer>,
    #[serde(default, rename = "roundResults", alias = "RoundResults")]
    rounds: Vec<RoundResult>,
}

#[derive(Deserialize, Default)]
struct RoundResult {
    #[serde(default, rename = "playerStats", alias = "PlayerStats")]
    players: Vec<RoundPlayer>,
}

#[derive(Deserialize, Default)]
struct RoundPlayer {
    #[serde(default, alias = "Subject")]
    subject: String,
    #[serde(default, rename = "damage", alias = "Damage")]
    damage: Vec<ShotDamage>,
}

#[derive(Deserialize, Default)]
struct ShotDamage {
    #[serde(default, alias = "Headshots")]
    headshots: u32,
    #[serde(default, alias = "Bodyshots")]
    bodyshots: u32,
    #[serde(default, alias = "Legshots")]
    legshots: u32,
}

#[derive(Deserialize)]
struct MatchPlayer {
    #[serde(default, alias = "Subject")]
    subject: String,
    #[serde(default, alias = "Stats")]
    stats: Option<MatchStats>,
}

#[derive(Deserialize)]
struct MatchStats {
    #[serde(default, alias = "Kills")]
    kills: u32,
    #[serde(default, alias = "Deaths")]
    deaths: u32,
}

#[derive(Deserialize)]
struct VersionResp {
    data: VersionData,
}

#[derive(Deserialize)]
struct VersionData {
    #[serde(rename = "riotClientVersion")]
    riot_client_version: String,
}

#[derive(Deserialize)]
struct SeasonsResp {
    data: Vec<Season>,
}

#[derive(Deserialize)]
struct Season {
    uuid: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "startTime")]
    start_time: Option<String>,
    #[serde(rename = "endTime")]
    end_time: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
struct SavedMeta {
    version: String,
    season_id: String,
    at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_parses_five_fields() {
        let lock = parse_lockfile("Riot Client:100:12345:abcDEF:https\n").unwrap();
        assert_eq!(lock.port, 12345);
        assert_eq!(lock.password, "abcDEF");
        assert_eq!(lock.protocol, "https");
    }

    #[test]
    fn shard_groups_americas() {
        assert_eq!(shard_for("LATAM"), "na");
        assert_eq!(shard_for("eu"), "eu");
        assert_eq!(shard_for("kr"), "kr");
        assert_eq!(shard_for("jp1"), "ap");
        assert_eq!(affinity("jp1"), Some(("ap", "ap")));
    }

    #[test]
    fn private_presence_decodes() {
        let json = br#"{"sessionLoopState":"PREGAME","matchMap":"/Game/Maps/Ascent/Ascent","queueId":"competitive"}"#;
        let encoded = base64::engine::general_purpose::STANDARD.encode(json);
        let private = decode_private(&encoded).unwrap();
        assert_eq!(phase_of(&private.session_loop_state), Phase::Pregame);
        assert_eq!(private.queue_id, "competitive");
        let nested = br#"{"matchPresenceData":{"sessionLoopState":"INGAME","matchMap":"/Game/Maps/Foxtrot/Foxtrot","queueId":"swiftplay"},"queueId":""}"#;
        let encoded = base64::engine::general_purpose::STANDARD.encode(nested);
        let private = decode_private(&encoded).unwrap();
        assert_eq!(phase_of(&private.session_loop_state), Phase::Ingame);
        assert_eq!(private.queue_id, "swiftplay");
        assert!(private.match_map.contains("Foxtrot"));
    }

    #[test]
    fn headshot_percent_uses_all_shots() {
        assert_eq!(shot_percent(1, 1, 2), Some(25));
        assert_eq!(shot_percent(0, 0, 0), None);
        let body = r#"{
            "players": [{"subject": "me", "stats": {"kills": 2, "deaths": 1}}],
            "roundResults": [{
                "playerStats": [{
                    "subject": "me",
                    "damage": [
                        {"headshots": 2, "bodyshots": 1, "legshots": 1},
                        {"headshots": 0, "bodyshots": 0, "legshots": 0}
                    ]
                }]
            }]
        }"#;
        let details: MatchDetails = serde_json::from_str(body).unwrap();
        assert_eq!(shots_for(&details, "me"), (2, 1, 1));
        assert_eq!(shot_percent(2, 1, 1), Some(50));
    }

    #[test]
    fn pregame_splits_teams() {
        let body = r#"{
            "MapID": "7eaecc1b-4337-bbf6-6ab9-04b8f06b3319",
            "QueueID": "competitive",
            "PhaseTimeRemainingNS": 45000000000,
            "Teams": [
                {"TeamID": "Blue", "Players": [
                    {"Subject": "me", "CharacterID": "add6443a-41bd-e414-f6ad-e58d267f4e95"}
                ]},
                {"TeamID": "Red", "Players": [
                    {"Subject": "them", "CharacterID": ""}
                ]}
            ],
            "AllyTeam": {"TeamID": "Blue", "Players": []}
        }"#;
        let roster = parse_pregame(body, "me", "", "").unwrap();
        assert_eq!(roster.slots.len(), 2);
        assert!(roster.slots[0].you && roster.slots[0].ally);
        assert_eq!(roster.slots[0].agent_id.contains("add6443a"), true);
        assert!(!roster.slots[1].ally);
        assert!(roster.timer_ends_ms.is_some());
    }

    #[test]
    fn core_uses_my_team() {
        let body = r#"{
            "MapID": "/Game/Maps/Port/Port",
            "QueueID": "unrated",
            "Players": [
                {"Subject": "me", "TeamID": "Red", "CharacterID": "add6443a-41bd-e414-f6ad-e58d267f4e95"},
                {"Subject": "them", "TeamID": "Blue", "CharacterID": "f94c3b30-42be-e959-889c-5aa313dba261"}
            ]
        }"#;
        let roster = parse_core(body, "me", "", "").unwrap();
        assert!(roster.slots[0].ally);
        assert!(!roster.slots[1].ally);
        assert_eq!(roster.phase, Phase::Ingame);
    }

    #[test]
    fn iso_epoch_and_2021() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1609459200), "2021-01-01T00:00:00Z");
    }

    #[test]
    fn form_reads_newest_on_the_right() {
        let updates = CompUpdates {
            matches: vec![
                CompMatch { match_id: "a".into(), season_id: "s".into(), rr_earned: 18 },
                CompMatch { match_id: "b".into(), season_id: "s".into(), rr_earned: -12 },
            ],
        };
        let (form, ids) = read_form(&updates, "s");
        assert_eq!(form, vec![-1, 1]);
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }
}
