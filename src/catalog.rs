pub fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

pub fn rank_short(tier: u8) -> &'static str {
    match tier {
        3 => "I1",
        4 => "I2",
        5 => "I3",
        6 => "B1",
        7 => "B2",
        8 => "B3",
        9 => "S1",
        10 => "S2",
        11 => "S3",
        12 => "G1",
        13 => "G2",
        14 => "G3",
        15 => "P1",
        16 => "P2",
        17 => "P3",
        18 => "D1",
        19 => "D2",
        20 => "D3",
        21 => "A1",
        22 => "A2",
        23 => "A3",
        24 => "IM1",
        25 => "IM2",
        26 => "IM3",
        27 => "RAD",
        _ => "UR",
    }
}

pub fn rank_color(tier: u8) -> u32 {
    match tier {
        3..=5 => rgb(168, 168, 168),
        6..=8 => rgb(186, 140, 96),
        9..=11 => rgb(196, 202, 206),
        12..=14 => rgb(214, 176, 84),
        15..=17 => rgb(72, 176, 168),
        18..=20 => rgb(176, 150, 230),
        21..=23 => rgb(96, 186, 120),
        24..=26 => rgb(214, 84, 104),
        27 => rgb(236, 214, 140),
        _ => rgb(150, 146, 138),
    }
}

pub fn queue_label(id: &str) -> String {
    match id.to_ascii_lowercase().as_str() {
        "competitive" => "COMP".into(),
        "unrated" => "UNR".into(),
        "swiftplay" => "SWIFT".into(),
        "spikerush" => "SPIKE".into(),
        "deathmatch" => "DM".into(),
        "ggteam" => "ESCAL".into(),
        "hurm" | "teamdeathmatch" => "TDM".into(),
        "premier" => "PREM".into(),
        "onefa" => "REP".into(),
        "snowball" => "SNOW".into(),
        "" | "custom" | "customgame" => "CUSTOM".into(),
        other => {
            let upper = other.to_ascii_uppercase();
            if upper.len() > 8 {
                upper[..8].to_string()
            } else {
                upper
            }
        }
    }
}

pub fn agent_name(id: &str) -> &'static str {
    let id = id.trim();
    if id.is_empty() || id.chars().all(|c| c == '0' || c == '-') {
        return "Pick";
    }
    AGENTS
        .iter()
        .find(|(uuid, _)| uuid.eq_ignore_ascii_case(id))
        .map(|(_, name)| *name)
        .unwrap_or("Agent")
}

pub fn map_name(id_or_path: &str) -> String {
    let raw = id_or_path.trim();
    if raw.is_empty() {
        return "Match".into();
    }
    for (uuid, name, _) in MAPS {
        if uuid.eq_ignore_ascii_case(raw) {
            return (*name).into();
        }
    }
    let lower = raw.to_ascii_lowercase();
    for (_, name, path) in MAPS {
        if !path.is_empty() && lower.contains(&path.to_ascii_lowercase()) {
            return (*name).into();
        }
    }
    let seg = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    if (2..24).contains(&seg.len()) {
        seg.to_string()
    } else {
        "Match".into()
    }
}

const AGENTS: &[(&str, &str)] = &[
    ("e370fa57-4757-3604-3648-499e1f642d3f", "Gekko"),
    ("dade69b4-4f5a-8528-247b-219e5a1facd6", "Fade"),
    ("5f8d3a7f-467b-97f3-062c-13acf203c006", "Breach"),
    ("cc8b64c8-4b25-4ff9-6e7f-37b4da43d235", "Deadlock"),
    ("b444168c-4e35-8076-db47-ef9bf368f384", "Tejo"),
    ("f94c3b30-42be-e959-889c-5aa313dba261", "Raze"),
    ("22697a3d-45bf-8dd7-4fec-84a9e28c69d7", "Chamber"),
    ("601dbbe7-43ce-be57-2a40-4abd24953621", "KAY/O"),
    ("6f2a04ca-43e0-be17-7f36-b3908627744d", "Skye"),
    ("117ed9e3-49f3-6512-3ccf-0cada7e3823b", "Cypher"),
    ("320b2a48-4d9b-a075-30f1-1f93a9b638fa", "Sova"),
    ("7c8a4701-4de6-9355-b254-e09bc2a34b72", "Miks"),
    ("1e58de9c-4950-5125-93e9-a0aee9f98746", "Killjoy"),
    ("95b78ed7-4637-86d9-7e41-71ba8c293152", "Harbor"),
    ("efba5359-4016-a1e5-7626-b1ae76895940", "Vyse"),
    ("707eab51-4836-f488-046a-cda6bf494859", "Viper"),
    ("eb93336a-449b-9c1b-0a54-a891f7921d69", "Phoenix"),
    ("92eeef5d-43b5-1d4a-8d03-b3927a09034b", "Veto"),
    ("41fb69c1-4189-7b37-f117-bcaf1e96f1bf", "Astra"),
    ("9f0d8ba9-4140-b941-57d3-a7ad57c6b417", "Brimstone"),
    ("0e38b510-41a8-5780-5e8f-568b2a4f2d6c", "Iso"),
    ("1dbf2edd-4729-0984-3115-daa5eed44993", "Clove"),
    ("bb2a4828-46eb-8cd1-e765-15848195d751", "Neon"),
    ("7f94d92c-4234-0a36-9646-3a87eb8b5c89", "Yoru"),
    ("df1cb487-4902-002e-5c17-d28e83e78588", "Waylay"),
    ("569fdd95-4d10-43ab-ca70-79becc718b46", "Sage"),
    ("a3bfb853-43b2-7238-a4f1-ad90e9e46bcc", "Reyna"),
    ("8e253930-4c05-31dd-1b6c-968525494517", "Omen"),
    ("add6443a-41bd-e414-f6ad-e58d267f4e95", "Jett"),
];

const MAPS: &[(&str, &str, &str)] = &[
    ("7eaecc1b-4337-bbf6-6ab9-04b8f06b3319", "Ascent", "/Game/Maps/Ascent/Ascent"),
    ("d960549e-485c-e861-8d71-aa9d1aed12a2", "Split", "/Game/Maps/Bonsai/Bonsai"),
    ("b529448b-4d60-346e-e89e-00a4c527a405", "Fracture", "/Game/Maps/Canyon/Canyon"),
    ("2c9d57ec-4431-9c5e-2939-8f9ef6dd5cba", "Bind", "/Game/Maps/Duality/Duality"),
    ("2fb9a4fd-47b8-4e7d-a969-74b4046ebd53", "Breeze", "/Game/Maps/Foxtrot/Foxtrot"),
    ("224b0a95-48b9-f703-1bd8-67aca101a61f", "Abyss", "/Game/Maps/Infinity/Infinity"),
    ("2fe4ed3a-450a-948b-6d6b-e89a78e680a9", "Lotus", "/Game/Maps/Jam/Jam"),
    ("92584fbe-486a-b1b2-9faa-39b0f486b498", "Sunset", "/Game/Maps/Juliett/Juliett"),
    ("fd267378-4d1d-484f-ff52-77821ed10dc2", "Pearl", "/Game/Maps/Pitt/Pitt"),
    ("e2ad5c54-4114-a870-9641-8ea21279579a", "Icebox", "/Game/Maps/Port/Port"),
    ("2bee0dc9-4ffe-519b-1cbd-7fbe763a6047", "Haven", "/Game/Maps/Triad/Triad"),
    ("1c18ab1f-420d-0d8b-71d0-77ad3c439115", "Corrode", "/Game/Maps/Rook/Rook"),
    ("756da597-416b-c0f2-f47b-afbdf28670bc", "Summit", "/Game/Maps/Plummet/Plummet"),
    ("690b3ed2-4dff-945b-8223-6da834e30d24", "District", "/Game/Maps/HURM/HURM_Alley/HURM_Alley"),
    ("12452a9d-48c3-0b02-e7eb-0381c3520404", "Kasbah", "/Game/Maps/HURM/HURM_Bowl/HURM_Bowl"),
    ("2c09d728-42d5-30d8-43dc-96a05cc7ee9d", "Drift", "/Game/Maps/HURM/HURM_Helix/HURM_Helix"),
    ("d6336a5a-428f-c591-98db-c8a291159134", "Glitch", "/Game/Maps/HURM/HURM_HighTide/HURM_HighTide"),
    ("de28aa9b-4cbe-1003-320e-6cb3ec309557", "Piazza", "/Game/Maps/HURM/HURM_Yard/HURM_Yard"),
    ("ee613ee9-28b7-4beb-9666-08db13bb2244", "Range", "/Game/Maps/Poveglia/Range"),
    ("5914d1e0-40c4-cfdd-6b88-eba06347686c", "Range", "/Game/Maps/PovegliaV2/RangeV2"),
];
